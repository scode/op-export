# op-export

A tool to export items from 1password into a human and tool readable *unencrypted* format. Combine with [saltybox](https://github.com/scode/saltybox) to get offline encrypted backups.

## Installation

* Install `op`, the official command line tool [provided by 1Password](https://1password.com/downloads/command-line/).
  At the time of this writing, version 0.9.3 has been tested.
* Install rust by following the instructions at https://rustup.rs/
* Install by running `cargo install --path .`

## Usage

Sign into 1password using `op`:

```
eval $(op signin)
```

Export all items in your vault to `~/tmp/1p.json` (will *not be encrypted*):

```
op-export -o ~/tmp/1p.json
```

Although `op-export` tries hard to let you know if anything goes wrong, it is recommended that the resulting file be inspected to ensure it looks reasonable. If `op get item` gives us valid JSON, we assume success and accept the JSON as a valid vault item. The `op` command has some strange failure modes and it is not clear whether it is meant to be used by automated tooling.

## Troubleshooting

The `op` tool has various strange failure modes, often different type of parsing errors rather than telling you what is actually wrong. Some modes of failure I have found require you to `op signin` again to unwedge it.