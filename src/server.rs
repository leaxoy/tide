use futures::{
    compat::{Compat, Future01CompatExt},
    future::{self, FutureObj},
    prelude::*,
};
use hyper::service::Service;
use std::{
    net::ToSocketAddrs,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use crate::{
    body::Body,
    extract::Extract,
    middleware::{logger::RootLogger, RequestContext},
    router::{Resource, RouteResult, Router},
    Middleware, Request, Response, RouteMatch,
};

/// The top-level type for setting up a Tide application.
///
/// Apps are equipped with a handle to their own state (`Data`), which is available to all endpoints.
/// This is a "handle" because it must be `Clone`, and endpoints are invoked with a fresh clone.
/// They also hold a top-level router.
pub struct ServerBuilder<Data> {
    data: Data,
    router: Router<Data>,
}

impl<Data: Clone + Send + Sync + 'static> ServerBuilder<Data> {
    /// Set up a new app with some initial `data`.
    pub fn new(data: Data) -> ServerBuilder<Data> {
        let logger = RootLogger::new();
        let mut builder = ServerBuilder {
            data,
            router: Router::new(),
        };

        // Add RootLogger as a default middleware
        builder.middleware(logger);
        builder
    }

    /// Get the top-level router.
    pub fn router(&mut self) -> &mut Router<Data> {
        &mut self.router
    }

    /// Add a new resource at `path`.
    pub fn at<'a>(&'a mut self, path: &'a str) -> Resource<'a, Data> {
        self.router.at(path)
    }

    /// Apply `middleware` to the whole app. Note that the order of nesting subrouters and applying
    /// middleware matters; see `Router` for details.
    pub fn middleware(&mut self, middleware: impl Middleware<Data> + 'static) -> &mut Self {
        self.router.middleware(middleware);
        self
    }

    /// Just call `Server.serve(addr: A)`
    pub fn serve<A: ToSocketAddrs>(self, addr: A) {
        Server::from(self).serve(addr)
    }
}

#[derive(Clone)]
pub struct Server<Data> {
    data: Data,
    router: Arc<Router<Data>>,
}

impl<Data: Clone + Send + Sync + 'static> Server<Data> {
    /// Start serving the app at the given address.
    ///
    /// Blocks the calling thread indefinitely.
    pub fn serve<A: ToSocketAddrs>(self, addr: A) {
        // TODO: be more robust
        let addr = addr.to_socket_addrs().unwrap().next().unwrap();

        let server = hyper::Server::bind(&addr)
            .serve(move || {
                let res: Result<_, std::io::Error> = Ok(self.clone());
                res
            })
            .compat()
            .map(|_| {
                let res: Result<(), ()> = Ok(());
                res
            })
            .compat();
        hyper::rt::run(server);
    }
}

impl<Data: Clone + Send + Sync + 'static> From<ServerBuilder<Data>> for Server<Data> {
    fn from(b: ServerBuilder<Data>) -> Self {
        Server {
            data: b.data,
            router: Arc::new(b.router),
        }
    }
}

impl<Data: Clone + Send + Sync + 'static> Service for Server<Data> {
    type ReqBody = hyper::Body;
    type ResBody = hyper::Body;
    type Error = std::io::Error;
    type Future = Compat<FutureObj<'static, Result<http::Response<hyper::Body>, Self::Error>>>;

    fn call(&mut self, req: http::Request<hyper::Body>) -> Self::Future {
        let data = self.data.clone();
        let router = self.router.clone();

        let req = req.map(Body::from);
        let path = req.uri().path().to_owned();
        let method = req.method().to_owned();

        FutureObj::new(Box::new(
            async move {
                if let Some(RouteResult {
                    endpoint,
                    params,
                    middleware,
                }) = router.route(&path, &method)
                {
                    let ctx = RequestContext {
                        app_data: data,
                        req,
                        params,
                        endpoint,
                        next_middleware: middleware,
                    };
                    let res = await!(ctx.next());
                    Ok(res.map(Into::into))
                } else {
                    Ok(http::Response::builder()
                        .status(http::status::StatusCode::NOT_FOUND)
                        .body(hyper::Body::empty())
                        .unwrap())
                }
            },
        ))
        .compat()
    }
}

/// An extractor for accessing app data.
///
/// Endpoints can use `AppData<T>` to gain a handle to the data (of type `T`) originally injected into their app.
pub struct AppData<T>(pub T);

impl<T> Deref for AppData<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for AppData<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: Clone + Send + 'static> Extract<T> for AppData<T> {
    type Fut = future::Ready<Result<Self, Response>>;
    fn extract(data: &mut T, req: &mut Request, params: &RouteMatch<'_>) -> Self::Fut {
        future::ok(AppData(data.clone()))
    }
}
