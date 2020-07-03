use anyhow::anyhow;
use crossbeam::channel::unbounded;
use crossbeam::channel::Receiver;
use crossbeam::channel::Sender;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

struct Item {
    uuid: String,
    body: String,
}

// Narrow trait representing the subset of functionality from the pp cmdline tool
// which we need.
//
// See also: https://1password.com/downloads/command-line/
trait Op: Send + Sync + 'static {
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

fn get_items(r: Receiver<String>, s: Sender<anyhow::Result<Item>>, op: Arc<dyn Op>) {
    for uuid in r {
        s.send(op.get_item(&uuid).map(|body| Item { uuid, body }))
            .unwrap();
    }
}

fn export(op: Arc<dyn Op>) -> anyhow::Result<()> {
    let (uuid_sender, uuid_receiver) = unbounded::<String>();
    let (item_sender, item_receiver) = unbounded::<anyhow::Result<Item>>();

    let mut getters: Vec<std::thread::JoinHandle<()>> = vec![];
    for _ in 0..5 {
        let opclone = op.clone();
        let rcvclone = uuid_receiver.clone();
        let sndclone = item_sender.clone();
        getters.push(thread::spawn(move || {
            get_items(rcvclone, sndclone, opclone);
        }));
    }
    drop(item_sender);

    for uuid in op.list_items().unwrap() {
        uuid_sender.send(uuid).unwrap();
    }

    drop(uuid_sender);

    for item in item_receiver {
        let item = item?;
        println!("item: {}: {}", item.uuid, item.body);
    }

    for getter in getters {
        getter.join().unwrap();
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    export(Arc::new(MockOp {
        items: [
            ("uuid1".to_owned(), "1body".to_owned()),
            ("uuid2".to_owned(), "2body".to_owned()),
        ]
        .iter()
        .cloned()
        .collect(),
    }))
}

mod test {}
