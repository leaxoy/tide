#![feature(async_await, futures_api)]

#[macro_use]
extern crate serde_derive;

use http::status::StatusCode;
use std::sync::{Arc, Mutex};
use tide::{body, head, ServerBuilder, AppData};

#[derive(Clone)]
struct Database {
    contents: Arc<Mutex<Vec<Message>>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Message {
    author: Option<String>,
    contents: String,
}

impl Database {
    fn new() -> Database {
        Database {
            contents: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn insert(&mut self, msg: Message) -> usize {
        let mut table = self.contents.lock().unwrap();
        table.push(msg);
        table.len() - 1
    }

    fn get(&mut self, id: usize) -> Option<Message> {
        self.contents.lock().unwrap().get(id).cloned()
    }

    fn set(&mut self, id: usize, msg: Message) -> bool {
        let mut table = self.contents.lock().unwrap();

        if let Some(old_msg) = table.get_mut(id) {
            *old_msg = msg;
            true
        } else {
            false
        }
    }
}

async fn new_message(mut db: AppData<Database>, msg: body::Json<Message>) -> String {
    db.insert(msg.0).to_string()
}

async fn set_message(
    mut db: AppData<Database>,
    id: head::Path<usize>,
    msg: body::Json<Message>,
) -> Result<(), StatusCode> {
    if db.set(id.0, msg.0) {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn get_message(
    mut db: AppData<Database>,
    id: head::Path<usize>,
) -> Result<body::Json<Message>, StatusCode> {
    if let Some(msg) = db.get(id.0) {
        Ok(body::Json(msg))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

fn main() {
    let mut app = ServerBuilder::new(Database::new());

    app.at("/message").post(new_message);
    app.at("/message/{}").get(get_message);
    app.at("/message/{}").post(set_message);

    app.serve("127.0.0.1:7878");
}
