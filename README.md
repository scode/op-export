# op-export

A tool to export items from [1Password](https://1password.com/) into a human and tool readable *unencrypted* format. Combine with [saltybox](https://github.com/scode/saltybox) to get an offline encrypted backup of your vault.

Make sure you have first evaluated the backup methods [offered by 1Password itself](https://support.1password.com/backups/) before deciding to use this tool.

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

Although `op-export` tries hard to let you know if anything goes wrong, it is recommended that the resulting file be inspected to ensure it looks reasonable.

`op-export` operates by invoking `op list items` followed by using `op get item` to fetch each item listed. If `op get item` gives us valid JSON, we assume success and accept the JSON as a valid vault item.

## Troubleshooting

The `op` tool has various strange failure modes, often different type of parsing errors rather than telling you what is actually wrong. Some modes of failure I have found require you to `op signin` again to unwedge it.