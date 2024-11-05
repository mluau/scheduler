# mlua_scheduler

An scheduler for mlua which uses the tokio runtime

## caveats

The scheduler yields infinitely when using tokio's runtime without the multi_thread flavor

## credits

Parts of the code is taken from lune's mlua-luau-scheduler crate
