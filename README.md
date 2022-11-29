<div align="center">
<h1>

Async adaptors for `perf-event`

</h1>

Async extension methods for the [`perf-event`] crate.

![ci-status](https://shields.io/github/checks-status/Phantomical/perf-event-async/master)
[![crates-io-version](https://shields.io/crates/v/perf-event-async)][crates-io]
[![crates-io-license](https://shields.io/crates/l/perf-event-async)][crates-io]
[![docs-rs-badge](https://shields.io/docsrs/perf-event-async)][docs-rs]

[`perf-event`]: https://crates.io/crates/perf-event
[crates-io]: https://crates.io/crates/perf-event-async
[docs-rs]: https://docs.rs/perf-event-async

</div>

---

This crate provides an extension trait for working with perf-event samplers
using async-await.

To use it, construct a `Sampler` as per usual, import the `AsyncSamplerExt`
trait and call `next_async` to asynchronously wait for the next sample to be
available.

```rust
use perf_event::{Builder, Sampler};
use perf_event::events::Software;
use perf_event::samples::SampleType;
use perf_event_async::AsyncSamplerExt;
use std::fs::File;
use std::io::Write;
use tokio::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut sampler = Builder::new()
        .kind(Software::CPU_CLOCK)
        .observe_self()
        .comm(true)
        .wakeup_watermark(1)
        .build_sampler(8192)
        .expect("Failed to construct sampler");

    sampler
        .enable()
        .expect("Failed to enable sampler");

    // Create a record in the sampler with a little delay
    let handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        File::create("/proc/self/comm")
            .unwrap()
            .write(b"example")
            .unwrap();
    });

    if let Some(record) = sampler.next_async().await {
      println!("{:#?}", record);
    }

    handle.await.unwrap();
}
```

# License
`perf-event-async` is distributed under the terms of both the MIT license and the Apache License (Version 2.0).

See [LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE) for details.