use anyhow;
use crossbeam::channel::unbounded;
use crossbeam::channel::Receiver;
use crossbeam::channel::Sender;
use std::sync::Arc;
use std::thread;

trait Op {
    fn list_items(&self) -> anyhow::Result<Vec<String>>;
    fn get_item(&self, uuid: &str) -> anyhow::Result<String>;
}

struct MockOp {}

impl Op for MockOp {
    fn list_items(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["itemuid".to_owned()])
    }

    fn get_item(&self, uuid: &str) -> anyhow::Result<String> {
        Ok("itembody".to_owned())
    }
}

fn get_items(r: Receiver<String>, s: Sender<anyhow::Result<String>>, op: Arc<dyn Op>) {
    for uuid in r {
        s.send(op.get_item(&uuid)).unwrap();
    }
}

fn main() {
    let op = Arc::new(MockOp {});

    let (uuid_sender, uuid_receiver) = unbounded::<String>();
    let (item_sender, item_receiver) = unbounded::<anyhow::Result<String>>();

    let clop = op.clone();
    let get_item_thread = thread::spawn(move || {
        get_items(uuid_receiver, item_sender, clop);
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
