//! Types for working directly with the bodies of requests and responses.
//!
//! This module includes types like `Json`, which can be used to automatically (de)serialize bodies
//! using `serde_json`.

use futures::{compat::Compat01As03, future::FutureObj, prelude::*, stream::StreamObj};
use http::status::StatusCode;
use multipart::server::Multipart;
use pin_utils::pin_mut;
use std::io::Cursor;

use crate::{Extract, IntoResponse, Request, Response, RouteMatch};

/// The raw contents of an http request or response.
///
/// A body is a stream of `BodyChunk`s, which are essentially `Vec<u8>` values.
/// Both `Body` and `BodyChunk` values can be easily created from standard byte buffer types,
/// using the `From` trait.
pub struct Body {
    inner: BodyInner,
}

type BodyStream = StreamObj<'static, Result<BodyChunk, Error>>;
type Error = Box<dyn std::error::Error + Send + Sync>;
pub struct BodyChunk(hyper::Chunk);

impl BodyChunk {
    pub fn as_bytes(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl From<Vec<u8>> for BodyChunk {
    fn from(v: Vec<u8>) -> Self {
        BodyChunk(v.into())
    }
}

impl From<String> for BodyChunk {
    fn from(v: String) -> Self {
        BodyChunk(v.into())
    }
}

enum BodyInner {
    Streaming(BodyStream),
    Fixed(Vec<u8>),
}

impl Body {
    /// Create an empty body.
    pub fn empty() -> Self {
        Body {
            inner: BodyInner::Fixed(Vec::new()),
        }
    }

    /// Collect the full contents of the body into a vector.
    ///
    /// This method is asynchronous because, in general, it requires reading an async
    /// stream of `BodyChunk` values.
    pub async fn read_to_vec(&mut self) -> Result<Vec<u8>, Error> {
        match &mut self.inner {
            BodyInner::Streaming(s) => {
                let mut bytes = Vec::new();
                pin_mut!(s);
                while let Some(chunk) = await!(s.next()) {
                    // TODO: do something more robust than `unwrap`
                    bytes.extend(chunk?.as_bytes());
                }
                Ok(bytes)
            }
            BodyInner::Fixed(v) => Ok(v.clone()),
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(v: Vec<u8>) -> Self {
        Self {
            inner: BodyInner::Fixed(v),
        }
    }
}

impl From<hyper::Body> for Body {
    fn from(body: hyper::Body) -> Body {
        // TODO: handle chunk-level errors
        let stream = Compat01As03::new(body).map(|c| match c {
            Ok(chunk) => Ok(BodyChunk(chunk)),
            Err(e) => {
                let e: Error = Box::new(e);
                Err(e)
            }
        });
        Body {
            inner: BodyInner::Streaming(StreamObj::new(Box::new(stream))),
        }
    }
}

impl From<BodyChunk> for hyper::Chunk {
    fn from(chunk: BodyChunk) -> hyper::Chunk {
        chunk.0
    }
}

impl Into<hyper::Body> for Body {
    fn into(self) -> hyper::Body {
        match self.inner {
            BodyInner::Fixed(v) => v.into(),
            BodyInner::Streaming(s) => hyper::Body::wrap_stream(s.compat()),
        }
    }
}

// Small utility function to return a stamped error when we cannot parse a request body
fn mk_err<T>(_: T) -> Response {
    StatusCode::BAD_REQUEST.into_response()
}

/// A wrapper for multipart form
///
/// This type is useable as an extractor (argument to an endpoint) for getting
/// a Multipart type defined in the multipart crate
pub struct MultipartForm(pub Multipart<Cursor<Vec<u8>>>);

impl<S: 'static> Extract<S> for MultipartForm {
    // Note: cannot use `existential type` here due to ICE
    type Fut = FutureObj<'static, Result<Self, Response>>;

    fn extract(data: &mut S, req: &mut Request, params: &RouteMatch<'_>) -> Self::Fut {
        // https://stackoverflow.com/questions/43424982/how-to-parse-multipart-forms-using-abonander-multipart-with-rocket

        const BOUNDARY: &str = "boundary=";
        let boundary = req.headers().get("content-type").and_then(|ct| {
            let ct = ct.to_str().ok()?;
            let idx = ct.find(BOUNDARY)?;
            Some(ct[idx + BOUNDARY.len()..].to_string())
        });

        let mut body = std::mem::replace(req.body_mut(), Body::empty());

        FutureObj::new(Box::new(
            async move {
                let body = await!(body.read_to_vec()).map_err(mk_err)?;
                let boundary = boundary.ok_or(()).map_err(mk_err)?;
                let mp = Multipart::with_body(Cursor::new(body), boundary);
                Ok(MultipartForm(mp))
            },
        ))
    }
}

/// A wrapper for json (de)serialization of bodies.
///
/// This type is usable both as an extractor (argument to an endpoint) and as a response
/// (return value from an endpoint).
pub struct Json<T>(pub T);

impl<T: Send + serde::de::DeserializeOwned + 'static, S: 'static> Extract<S> for Json<T> {
    // Note: cannot use `existential type` here due to ICE
    type Fut = FutureObj<'static, Result<Self, Response>>;

    fn extract(data: &mut S, req: &mut Request, params: &RouteMatch<'_>) -> Self::Fut {
        let mut body = std::mem::replace(req.body_mut(), Body::empty());
        FutureObj::new(Box::new(
            async move {
                let body = await!(body.read_to_vec()).map_err(mk_err)?;
                let json: T = serde_json::from_slice(&body).map_err(mk_err)?;
                Ok(Json(json))
            },
        ))
    }
}

impl<T: 'static + Send + serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        // TODO: think about how to handle errors
        http::Response::builder()
            .status(http::status::StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&self.0).unwrap()))
            .unwrap()
    }
}

/// A wrapper for form encoded (application/x-www-form-urlencoded) (de)serialization of bodies.
///
/// This type is usable both as an extractor (argument to an endpoint) and as a response
/// (return value from an endpoint), though returning a response with form data is uncommon
/// and probably not good practice.
pub struct Form<T>(pub T);

impl<T: Send + serde::de::DeserializeOwned + 'static, S: 'static> Extract<S> for Form<T> {
    // Note: cannot use `existential type` here due to ICE
    type Fut = FutureObj<'static, Result<Self, Response>>;

    fn extract(data: &mut S, req: &mut Request, params: &RouteMatch<'_>) -> Self::Fut {
        let mut body = std::mem::replace(req.body_mut(), Body::empty());
        FutureObj::new(Box::new(
            async move {
                let body = await!(body.read_to_vec()).map_err(mk_err)?;
                let data: T = serde_qs::from_bytes(&body).map_err(mk_err)?;
                Ok(Form(data))
            },
        ))
    }
}

impl<T: 'static + Send + serde::Serialize> IntoResponse for Form<T> {
    fn into_response(self) -> Response {
        // TODO: think about how to handle errors
        http::Response::builder()
            .status(http::status::StatusCode::OK)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(
                serde_qs::to_string(&self.0).unwrap().into_bytes(),
            ))
            .unwrap()
    }
}

pub struct Str(pub String);

impl<S: 'static> Extract<S> for Str {
    type Fut = FutureObj<'static, Result<Self, Response>>;

    fn extract(data: &mut S, req: &mut Request, params: &RouteMatch<'_>) -> Self::Fut {
        let mut body = std::mem::replace(req.body_mut(), Body::empty());

        FutureObj::new(Box::new(
            async move {
                let body = await!(body.read_to_vec().map_err(mk_err))?;
                let string = String::from_utf8(body).map_err(mk_err)?;
                Ok(Str(string))
            },
        ))
    }
}

pub struct StrLossy(pub String);

impl<S: 'static> Extract<S> for StrLossy {
    type Fut = FutureObj<'static, Result<Self, Response>>;

    fn extract(data: &mut S, req: &mut Request, params: &RouteMatch<'_>) -> Self::Fut {
        let mut body = std::mem::replace(req.body_mut(), Body::empty());

        FutureObj::new(Box::new(
            async move {
                let body = await!(body.read_to_vec().map_err(mk_err))?;
                let string = String::from_utf8_lossy(&body).to_string();
                Ok(StrLossy(string))
            },
        ))
    }
}

pub struct Bytes(pub Vec<u8>);

impl<S: 'static> Extract<S> for Bytes {
    type Fut = FutureObj<'static, Result<Self, Response>>;

    fn extract(data: &mut S, req: &mut Request, params: &RouteMatch<'_>) -> Self::Fut {
        let mut body = std::mem::replace(req.body_mut(), Body::empty());

        FutureObj::new(Box::new(
            async move {
                let body = await!(body.read_to_vec().map_err(mk_err))?;
                Ok(Bytes(body))
            },
        ))
    }
}
