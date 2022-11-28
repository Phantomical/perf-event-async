//! This crate provides methods for working with perf-event [`Sampler`]s in an
//! [`tokio`] async context.
//!
//! To extend [`Sampler`] with the methods from this crate, import the
//! [`AsyncSamplerExt`] trait:
//!
//! ```
//! use perf_event_async::AsyncSamplerExt;
//! ```
//!
//! Look at the docs for [`AsyncSamplerExt`] to see which extension methods are
//! provided.

#![warn(missing_docs)]

use std::future::Future;
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use perf_event::samples::Record;
use perf_event::Sampler;
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;

use crate::sealed::Sealed;

mod sealed {
  use super::*;

  pub trait Sealed {}

  impl Sealed for Sampler {}
}

/// Async extension methods for [`Sampler`].
pub trait AsyncSamplerExt: Sealed {
  /// Read the next record, blocking until one is ready.
  ///
  /// This method will only return `None` in cases where the kernel knows that
  /// there will never be another event emitted into the ring buffer. This only
  /// happens in one specific case: a [`Sampler`] is monitoring a single PID and
  /// the corresponding process exits. In all other cases, the stream will keep
  /// going indefinitely.
  ///
  /// Note that disabling a counter will not cause `next_async` to exit. So the
  /// code below will block forever.
  ///
  /// ```
  /// # use perf_event::Builder;
  /// # use perf_event_async::AsyncSamplerExt;
  /// # async {
  /// # let mut sampler = Builder::new().build_sampler(8192)?;
  /// sampler.disable()?;
  /// let _ = sampler.next_async().await;
  /// # Ok::<_, std::io::Error>(())
  /// # };
  /// ```
  ///
  /// # Example
  /// ```
  /// # use perf_event::Builder;
  /// # async {
  /// use perf_event_async::AsyncSamplerExt;
  ///
  /// let mut sampler = Builder::new()
  ///   // ...
  ///   .build_sampler(16384)?;
  ///
  /// while let Some(record) = sampler.next_async().await {
  ///   println!("{:?}", record);
  /// }
  ///
  /// #   Ok::<(), std::io::Error>(())
  /// # };
  /// ```
  ///
  /// # Panics
  /// This method will panic if called outside of a tokio runtime context.
  fn next_async(&mut self) -> Next;

  /// Get this [`Sampler`] as a [`Stream`] of [`Record`]s.
  ///
  /// This allows a [`Sampler`] to be used as part of the existing futures
  /// stream ecosystem.
  ///
  /// # Panics
  /// This method will panic if called outside of a tokio runtime context.
  fn stream(self) -> RecordStream;
}

impl AsyncSamplerExt for Sampler {
  fn next_async(&mut self) -> Next {
    Next::new(self)
  }

  fn stream(self) -> RecordStream {
    RecordStream::new(self)
  }
}

/// Future type for [`AsyncSamplerExt::next_async`].
pub struct Next<'a>(AsyncFd<SamplerRef<'a>>);

impl<'a> Next<'a> {
  fn new(sampler: &'a mut Sampler) -> Self {
    Self(
      AsyncFd::with_interest(SamplerRef(sampler), Interest::READABLE)
        .expect("unable to create asyncfd for sampler"),
    )
  }
}

/// Wrapper around [`Sampler`] that also implements [`Stream`].
///
/// See [`AsyncSamplerExt::stream`].
pub struct RecordStream(AsyncFd<Sampler>);

impl RecordStream {
  fn new(sampler: Sampler) -> Self {
    let asyncfd = AsyncFd::with_interest(sampler, Interest::READABLE) //
      .expect("unable to create asyncfd");

    Self(asyncfd)
  }

  /// Access the contained sampler.
  pub fn inner(&self) -> &Sampler {
    self.0.get_ref()
  }

  /// Mutably access the contained sampler.
  pub fn inner_mut(&mut self) -> &mut Sampler {
    self.0.get_mut()
  }

  /// Go back from the `RecordStream` and recover the inner [`Sampler`]
  /// instance.
  pub fn into_inner(self) -> Sampler {
    self.0.into_inner()
  }
}

struct SamplerRef<'a>(&'a mut Sampler);

impl AsRawFd for SamplerRef<'_> {
  fn as_raw_fd(&self) -> RawFd {
    self.0.as_raw_fd()
  }
}

trait SamplePollTarget: AsRawFd {
  fn next_record(&mut self) -> Option<Record>;
}

impl SamplePollTarget for Sampler {
  fn next_record(&mut self) -> Option<Record> {
    self.next_record()
  }
}

impl<'a> SamplePollTarget for SamplerRef<'a> {
  fn next_record(&mut self) -> Option<Record> {
    self.0.next_record()
  }
}

trait SamplePoll: Unpin {
  type Inner: SamplePollTarget;

  fn asyncfd(&mut self) -> &mut AsyncFd<Self::Inner>;

  fn poll_next_sample(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Record>> {
    loop {
      return match self.asyncfd().poll_read_ready_mut(cx) {
        Poll::Pending => Poll::Pending,
        Poll::Ready(Err(e)) => panic!("polling asyncfd returned an error: {}", e),
        Poll::Ready(Ok(mut guard)) => {
          if let Some(record) = guard.get_inner_mut().next_record() {
            return Poll::Ready(Some(record));
          }

          let fd = guard.get_ref().as_raw_fd();

          let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
          };

          // Need to use libc::poll to tell the difference between POLLIN and POLLHUP.
          match unsafe { libc::poll(&mut pollfd, 1, 0) } {
            // Poll indicates that no events are ready.
            0 => {
              guard.clear_ready();
              Poll::Pending
            }

            // Got an edge-case error. Keep readiness and return pending and hope it'll be resolved
            // with the next poll.
            //
            // Note that the only possible error cases for poll(2) are
            // - EINTR  - transient and retrying will just work. Note that since we use a timeout of
            //   0 this shouldn't be possible anyway.
            // - EFAULT - the pointer passed to poll was invalid. This will never happen in the code
            //   above.
            // - EINVAL - allocating one new fd exceeds RLIMIT_NOFILE
            // - ENOMEM - the kernel could not allocate memory for required data structures
            //
            // Neither of the last two are likely to ever happen so busy-waiting is probably a
            // reasonable approach.
            -1 => {
              guard.retain_ready();
              Poll::Pending
            }

            // The sampler was tracking a single other process and that process has exited.
            //
            // However, there may still be events in the sampler ring buffer so in this case we
            // still need to check.
            1 if pollfd.revents & libc::POLLHUP != 0 => {
              guard.clear_ready();
              Poll::Ready(guard.get_inner_mut().next_record())
            }

            // Somehow, the file descriptor has been closed. In this case, panic.
            1 if pollfd.revents & libc::POLLNVAL != 0 => {
              panic!("sampler file descriptor has been closed unexpectedly");
            }

            // Got POLLIN. This means that a message has arrived in the ringbuffer between the tokio
            // read and our handling of the future. Preserve the readiness status and go back around
            // to retry the poll.
            1 => {
              guard.retain_ready();
              continue;
            }

            // poll returning anything other than -1, 0, or 1 means that something is seriously
            // wrong.
            _ => unreachable!(),
          }
        }
      };
    }
  }
}

impl<'a> SamplePoll for Next<'a> {
  type Inner = SamplerRef<'a>;

  fn asyncfd(&mut self) -> &mut AsyncFd<Self::Inner> {
    &mut self.0
  }
}

impl Future for Next<'_> {
  type Output = Option<Record>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    self.poll_next_sample(cx)
  }
}

impl SamplePoll for RecordStream {
  type Inner = Sampler;

  fn asyncfd(&mut self) -> &mut AsyncFd<Self::Inner> {
    &mut self.0
  }
}

impl Stream for RecordStream {
  type Item = Record;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    self.poll_next_sample(cx)
  }
}

impl AsRawFd for RecordStream {
  fn as_raw_fd(&self) -> RawFd {
    self.0.as_raw_fd()
  }
}

impl IntoRawFd for RecordStream {
  fn into_raw_fd(self) -> RawFd {
    self.into_inner().into_raw_fd()
  }
}
