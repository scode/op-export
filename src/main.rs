use anyhow::anyhow;
use crossbeam::channel::unbounded;
use crossbeam::channel::Receiver;
use crossbeam::channel::Sender;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

// Narrow trait representing the subset of functionality from the pp cmdline tool
// which we need.
//
// See also: https://1password.com/downloads/command-line/
trait Op {
    fn list_items(&self) -> anyhow::Result<Vec<String>>;
    fn get_item(&self, uuid: &str) -> anyhow::Result<String>;
}

struct MockOp {
    // uuid -> item body
    items: HashMap<String, String>,
}

impl Op for MockOp {
    fn list_items(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.items.keys().map(|s| s.to_owned()).collect())
    }

    fn get_item(&self, uuid: &str) -> anyhow::Result<String> {
        match self.items.get(uuid) {
            Some(body) => Ok(body.to_owned()),
            None => Err(anyhow!("no item by this uuid: {}", uuid)),
        }
    }
}

fn get_items(r: Receiver<String>, s: Sender<anyhow::Result<String>>, op: Arc<dyn Op>) {
    for uuid in r {
        s.send(op.get_item(&uuid)).unwrap();
    }
}

// TODO: Arc<Op>
fn export(op: Arc<MockOp>) {
    let (uuid_sender, uuid_receiver) = unbounded::<String>();
    let (item_sender, item_receiver) = unbounded::<anyhow::Result<String>>();

    let opclone = op.clone();
    let get_item_thread = thread::spawn(move || {
        get_items(uuid_receiver, item_sender, opclone);
    });

    for uuid in op.list_items().unwrap() {
        uuid_sender.send(uuid).unwrap();
    }

    drop(uuid_sender);

    for item in item_receiver {
        println!("item: {}", item.unwrap());
    }

    get_item_thread.join().unwrap();
}

fn main() {
    export(Arc::new(MockOp {
        items: [
            ("uuid1".to_owned(), "1body".to_owned()),
            ("uuid2".to_owned(), "2body".to_owned()),
        ]
        .iter()
        .cloned()
        .collect(),
    }));
}

mod test {}
