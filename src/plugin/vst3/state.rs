//! A VST3 plugin's state, as the host keeps it (step 112, part C).
//!
//! A plugin saves what it is (the sound a person picked in its window, a model it loaded) into a stream that the host gives it (`IComponent::getState`, and `IEditController::getState` for what only the controller knows),
//! and is given it back the same way (`setState`, and for a plugin whose controller is not its component `setComponentState`, which brings the controller in step). Tang keeps the two as one blob of bytes that it does not
//! read: a little container (`encode`, `decode`) round the two streams, which a session file keeps as text (base64).
//!
//! What a plugin is asked, and when, is the format's: the stream is a memory stream of ours (`TangStream`, an `IBStream`), a plugin may seek in it and read it more than once, and it is made new for each call. The plugin's calls
//! are closures here, so that what is Tang's (the stream, the container, the order, what is let go of) can be held by tests that have no plugin.

use std::ffi::c_void;
use std::sync::Mutex;

use vst3::Steinberg::IBStream_::IStreamSeekMode_::{kIBSeekCur, kIBSeekEnd, kIBSeekSet};
use vst3::Steinberg::{IBStream, IBStreamTrait, kInvalidArgument, kOutOfMemory, kResultFalse, kResultOk, tresult};
use vst3::{Class, ComPtr, ComWrapper};

use crate::plugin::MAX_STATE_BYTES;

// (The format's names for where a seek is from, as the `i32` that the stream's `seek` is given.)
const SEEK_SET: i32 = kIBSeekSet as i32;
const SEEK_CUR: i32 = kIBSeekCur as i32;
const SEEK_END: i32 = kIBSeekEnd as i32;

/// A memory stream for a plugin to write its state to, or to read it from.
#[derive(Default)]
pub(super) struct TangStream {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    data: Vec<u8>,
    at: usize,
}

impl TangStream {
    /// A stream that has `bytes` in it, to be read from the start.
    fn with(bytes: &[u8]) -> Self {
        TangStream { inner: Mutex::new(Inner { data: bytes.to_vec(), at: 0 }) }
    }

    /// Everything that has been written to it.
    fn bytes(&self) -> Vec<u8> {
        self.inner.lock().map(|inner| inner.data.clone()).unwrap_or_default()
    }
}

impl Class for TangStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for TangStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: i32, num_bytes_read: *mut i32) -> tresult {
        if num_bytes < 0 || (buffer.is_null() && num_bytes > 0) {
            return kInvalidArgument;
        }
        let Ok(mut inner) = self.inner.lock() else { return kInvalidArgument };
        let wanted = num_bytes as usize;
        let n = wanted.min(inner.data.len() - inner.at);
        if n > 0 {
            // Safety: the plugin says that `buffer` has room for `num_bytes`, and `n` is no more than that and no more than there is.
            unsafe { std::ptr::copy_nonoverlapping(inner.data[inner.at..].as_ptr(), buffer.cast::<u8>(), n) };
            inner.at += n;
        }
        if !num_bytes_read.is_null() {
            // Safety: a plugin that gives a place for the count gives a good one.
            unsafe { *num_bytes_read = n as i32 };
        }
        // (The end of the stream is said as the SDK's own memory stream says it: nothing was read, and the result is false.)
        if n == 0 && wanted > 0 { kResultFalse } else { kResultOk }
    }

    unsafe fn write(&self, buffer: *mut c_void, num_bytes: i32, num_bytes_written: *mut i32) -> tresult {
        if num_bytes < 0 || (buffer.is_null() && num_bytes > 0) {
            return kInvalidArgument;
        }
        let Ok(mut inner) = self.inner.lock() else { return kInvalidArgument };
        let n = num_bytes as usize;
        let end = inner.at + n;
        if end > MAX_STATE_BYTES {
            log::warn!("a VST3 plugin wrote more state than the {MAX_STATE_BYTES} bytes that are kept: it is refused");
            return kOutOfMemory;
        }
        if inner.data.len() < end {
            inner.data.resize(end, 0);
        }
        if n > 0 {
            let at = inner.at;
            // Safety: the plugin says that `buffer` has `num_bytes` to give, and the data has room for them from `at`.
            unsafe { std::ptr::copy_nonoverlapping(buffer.cast::<u8>(), inner.data[at..].as_mut_ptr(), n) };
        }
        inner.at = end;
        if !num_bytes_written.is_null() {
            // Safety: as for `read`.
            unsafe { *num_bytes_written = num_bytes };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let Ok(mut inner) = self.inner.lock() else { return kInvalidArgument };
        let from = match mode {
            SEEK_SET => 0,
            SEEK_CUR => inner.at as i64,
            SEEK_END => inner.data.len() as i64,
            _ => return kInvalidArgument,
        };
        // (Kept between the start and the end, as the SDK's own memory stream keeps it.)
        inner.at = from.saturating_add(pos).clamp(0, inner.data.len() as i64) as usize;
        if !result.is_null() {
            // Safety: as for `read`.
            unsafe { *result = inner.at as i64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        let Ok(inner) = self.inner.lock() else { return kInvalidArgument };
        if pos.is_null() {
            return kInvalidArgument;
        }
        // Safety: as for `read`.
        unsafe { *pos = inner.at as i64 };
        kResultOk
    }
}

/// A stream with `bytes` in it, as the pointer that a plugin is given, and what holds it up.
fn stream_with(bytes: &[u8]) -> Option<(ComWrapper<TangStream>, ComPtr<IBStream>)> {
    let stream = ComWrapper::new(TangStream::with(bytes));
    let ptr = stream.to_com_ptr::<IBStream>()?;
    Some((stream, ptr))
}

const MAGIC: &[u8; 4] = b"TV3S";
const VERSION: u8 = 1;

/// The two states as one blob: a mark and a version, and then the component's and the controller's, each after its length (four bytes, little endian).
pub(super) fn encode(component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(MAGIC.len() + 1 + 8 + component.len() + controller.len());
    blob.extend_from_slice(MAGIC);
    blob.push(VERSION);
    for part in [component, controller] {
        blob.extend_from_slice(&(part.len() as u32).to_le_bytes());
        blob.extend_from_slice(part);
    }
    blob
}

/// The component's and the controller's state in a blob that `encode` made; the reason in words if it is not one (a file that was edited, or made by something else).
pub(super) fn decode(blob: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let rest = blob.strip_prefix(MAGIC.as_slice()).ok_or("this is not the state of a VST3 plugin that Tang kept")?;
    let (&version, mut rest) = rest.split_first().ok_or("the state has nothing after its mark")?;
    if version != VERSION {
        return Err(format!("the state is of version {version}, and this Tang reads version {VERSION}"));
    }
    let mut parts = [&[][..]; 2];
    for part in &mut parts {
        let (length, after) = rest.split_first_chunk::<4>().ok_or("the state ends in the middle of a length")?;
        let length = u32::from_le_bytes(*length) as usize;
        if length > after.len() {
            return Err(format!("the state says {length} bytes and has {}", after.len()));
        }
        (*part, rest) = after.split_at(length);
    }
    if !rest.is_empty() {
        return Err(format!("the state has {} bytes after its end", rest.len()));
    }
    Ok((parts[0], parts[1]))
}

/// The plugin's state, as one blob, if it has any: `get_component` asks the component for its state (`IComponent::getState`) and `get_controller` the controller for its own (`IEditController::getState`), each given a stream to
/// write to. A component that gives none, or a result that is not ok, is a plugin that has no state to keep (the controller's alone is not kept, it has nothing to be in step with); a controller that gives none is left out.
pub(super) fn capture(get_component: impl FnOnce(*mut IBStream) -> tresult, get_controller: impl FnOnce(*mut IBStream) -> tresult) -> Option<Vec<u8>> {
    let (component, component_ptr) = stream_with(&[])?;
    if get_component(component_ptr.as_ptr()) != kResultOk {
        return None;
    }
    let (controller, controller_ptr) = stream_with(&[])?;
    let controller_bytes = if get_controller(controller_ptr.as_ptr()) == kResultOk { controller.bytes() } else { Vec::new() };
    let component_bytes = component.bytes();
    if component_bytes.is_empty() {
        return None;
    }
    Some(encode(&component_bytes, &controller_bytes))
}

/// Give the plugin a state that `capture` made: `set_component` gives the component its state (`IComponent::setState`), `sync_controller` the controller the component's state, to bring it in step with it
/// (`IEditController::setComponentState`: for a plugin whose controller is not its component), and `set_controller` the controller its own, if there was one (`IEditController::setState`). Each is given a stream of its own that
/// is read from the start. The reason in words if the blob is not one or the component does not take it; what the controller says is not worth refusing the rest for.
pub(super) fn restore(
    blob: &[u8],
    set_component: impl FnOnce(*mut IBStream) -> tresult,
    sync_controller: impl FnOnce(*mut IBStream) -> tresult,
    set_controller: impl FnOnce(*mut IBStream) -> tresult,
) -> Result<(), String> {
    let (component_bytes, controller_bytes) = decode(blob)?;
    let (_component, component_ptr) = stream_with(component_bytes).ok_or("a stream for the state could not be made")?;
    let said = set_component(component_ptr.as_ptr());
    if said != kResultOk {
        return Err(format!("the plugin did not take its state (result {said})"));
    }
    if let Some((_stream, ptr)) = stream_with(component_bytes) {
        let said = sync_controller(ptr.as_ptr());
        if said != kResultOk {
            log::debug!("VST3 controller did not take the component's state (result {said})");
        }
    }
    if !controller_bytes.is_empty()
        && let Some((_stream, ptr)) = stream_with(controller_bytes)
    {
        let said = set_controller(ptr.as_ptr());
        if said != kResultOk {
            log::debug!("VST3 controller did not take its own state (result {said})");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use vst3::ComRef;

    use super::*;

    /// What a plugin does with the stream it is given: writes to it, through the pointer, as a plugin would.
    fn write_to(stream: *mut IBStream, bytes: &[u8]) -> tresult {
        // Safety: the pointer is the one that `capture` made of a stream that is alive for the call.
        let stream = unsafe { ComRef::from_raw(stream) }.expect("a stream");
        let mut written = 0;
        unsafe { stream.write(bytes.as_ptr() as *mut c_void, bytes.len() as i32, &mut written) }
    }

    /// Reads all of it, as a plugin does: until the stream says that there is no more.
    fn read_all(stream: *mut IBStream) -> Vec<u8> {
        // Safety: as above.
        let stream = unsafe { ComRef::from_raw(stream) }.expect("a stream");
        let mut all = Vec::new();
        loop {
            let mut buffer = [0u8; 4];
            let mut read = 0;
            let said = unsafe { stream.read(buffer.as_mut_ptr().cast(), buffer.len() as i32, &mut read) };
            all.extend_from_slice(&buffer[..read as usize]);
            if said != kResultOk || read == 0 {
                return all;
            }
        }
    }

    #[test]
    fn a_stream_is_written_read_back_from_where_the_plugin_seeks_and_says_where_the_end_is() {
        let stream = TangStream::default();
        let (mut written, mut at) = (0, 0);
        unsafe {
            assert_eq!(stream.write(b"hello world".as_ptr() as *mut c_void, 11, &mut written), kResultOk);
            assert_eq!((written, stream.tell(&mut at), at), (11, kResultOk, 11));
            // Back to the start, and read in two parts.
            assert_eq!(stream.seek(0, SEEK_SET, &mut at), kResultOk);
            let mut buffer = [0u8; 16];
            let mut read = 0;
            assert_eq!(stream.read(buffer.as_mut_ptr().cast(), 5, &mut read), kResultOk);
            assert_eq!((&buffer[..5], read), (&b"hello"[..], 5));
            // More is asked for than there is: what there is, and then the end, which the SDK says as false.
            assert_eq!(stream.read(buffer.as_mut_ptr().cast(), 16, &mut read), kResultOk);
            assert_eq!((&buffer[..6], read), (&b" world"[..], 6));
            assert_eq!(stream.read(buffer.as_mut_ptr().cast(), 16, &mut read), kResultFalse);
            assert_eq!(read, 0);
            // An empty read at the end is not the end.
            assert_eq!(stream.read(buffer.as_mut_ptr().cast(), 0, &mut read), kResultOk);
            // Writing in the middle overwrites, and past the end makes it longer.
            assert_eq!(stream.seek(6, SEEK_SET, &mut at), kResultOk);
            assert_eq!(stream.write(b"WORLD!!".as_ptr() as *mut c_void, 7, &mut written), kResultOk);
        }
        assert_eq!(stream.bytes(), b"hello WORLD!!");
    }

    #[test]
    fn a_seek_is_from_the_start_the_place_or_the_end_and_is_kept_between_the_start_and_the_end() {
        let stream = TangStream::with(b"0123456789");
        let mut at = -1;
        unsafe {
            assert_eq!((stream.seek(4, SEEK_SET, &mut at), at), (kResultOk, 4));
            assert_eq!((stream.seek(3, SEEK_CUR, &mut at), at), (kResultOk, 7));
            assert_eq!((stream.seek(-2, SEEK_END, &mut at), at), (kResultOk, 8));
            assert_eq!((stream.seek(-100, SEEK_CUR, &mut at), at), (kResultOk, 0), "not before the start");
            assert_eq!((stream.seek(100, SEEK_SET, &mut at), at), (kResultOk, 10), "not after the end");
            assert_eq!((stream.seek(i64::MAX, SEEK_CUR, &mut at), at), (kResultOk, 10), "and a number that would not add up is the end");
            assert_eq!(stream.seek(0, 7, &mut at), kInvalidArgument, "a way of seeking that there is not");
            assert_eq!(at, 10, "and the place is as it was");
            assert_eq!(stream.tell(&mut at), kResultOk);
            assert_eq!(stream.tell(std::ptr::null_mut()), kInvalidArgument);
        }
    }

    #[test]
    fn a_stream_refuses_what_cannot_be_read_or_written_and_more_state_than_is_kept() {
        let stream = TangStream::with(b"abc");
        let mut buffer = [0u8; 4];
        unsafe {
            assert_eq!(stream.read(buffer.as_mut_ptr().cast(), -1, std::ptr::null_mut()), kInvalidArgument);
            assert_eq!(stream.read(std::ptr::null_mut(), 2, std::ptr::null_mut()), kInvalidArgument);
            assert_eq!(stream.write(std::ptr::null_mut(), 2, std::ptr::null_mut()), kInvalidArgument);
            assert_eq!(stream.write(buffer.as_mut_ptr().cast(), -1, std::ptr::null_mut()), kInvalidArgument);
            // (The counts are for a plugin that wants them: a plugin that does not ask for them is answered all the same.)
            assert_eq!(stream.read(buffer.as_mut_ptr().cast(), 2, std::ptr::null_mut()), kResultOk);
            // One byte too many for what a state may be.
            let mut at = 0;
            assert_eq!(stream.seek(0, SEEK_END, &mut at), kResultOk);
            let mut written = 0;
            assert_eq!(stream.write(buffer.as_mut_ptr().cast(), 0, &mut written), kResultOk, "nothing is always room");
            let big = TangStream::default();
            let block = vec![7u8; MAX_STATE_BYTES];
            assert_eq!(big.write(block.as_ptr() as *mut c_void, MAX_STATE_BYTES as i32, &mut written), kResultOk, "all that is kept");
            assert_eq!(big.write(block.as_ptr() as *mut c_void, 1, &mut written), kOutOfMemory, "and no more");
        }
        assert_eq!(stream.bytes(), b"abc", "the refusals changed nothing");
    }

    #[test]
    fn a_blob_gives_back_the_two_states_that_made_it_and_says_what_is_wrong_with_one_that_is_not() {
        let blob = encode(b"component", b"controller");
        assert_eq!(decode(&blob), Ok((&b"component"[..], &b"controller"[..])));
        assert_eq!(decode(&encode(b"", b"c")), Ok((&b""[..], &b"c"[..])), "a part may be empty");
        assert_eq!(&blob[..5], b"TV3S\x01", "the mark and the version come first");
        assert_eq!(blob.len(), 5 + 4 + 9 + 4 + 10);
        for (what, bad, why) in [
            ("nothing", Vec::new(), "not the state"),
            ("another mark", b"CLAP\x01\0\0\0\0\0\0\0\0".to_vec(), "not the state"),
            ("only the mark", b"TV3S".to_vec(), "nothing after its mark"),
            ("another version", [b"TV3S\x02".as_slice(), &blob[5..]].concat(), "version 2"),
            ("a length cut short", blob[..7].to_vec(), "middle of a length"),
            ("a body cut short", blob[..blob.len() - 1].to_vec(), "says 10 bytes and has 9"),
            ("bytes after the end", [blob.as_slice(), b"x"].concat(), "1 bytes after its end"),
        ] {
            let error = decode(&bad).expect_err(what);
            assert!(error.contains(why), "{what}: {error}");
        }
    }

    #[test]
    fn a_plugins_state_is_captured_from_what_it_writes_and_a_plugin_with_none_has_none() {
        let both = capture(|stream| write_to(stream, b"the sound"), |stream| write_to(stream, b"the view"));
        assert_eq!(both.as_deref(), Some(encode(b"the sound", b"the view").as_slice()));
        // A controller that has nothing to say, or says no, is left out; the state is the component's.
        let alone = capture(|stream| write_to(stream, b"the sound"), |_| kResultFalse);
        assert_eq!(alone.as_deref(), Some(encode(b"the sound", b"").as_slice()));
        // A component that says no, or writes nothing, has no state to keep, whatever the controller would say.
        assert_eq!(capture(|_| kResultFalse, |stream| write_to(stream, b"the view")), None);
        assert_eq!(capture(|_| kResultOk, |stream| write_to(stream, b"the view")), None);
    }

    #[test]
    fn a_state_is_given_back_to_the_component_and_the_controller_each_from_the_start_of_a_stream_of_its_own() {
        let blob = encode(b"the sound", b"the view");
        let (mut component, mut synced, mut controller) = (Vec::new(), Vec::new(), Vec::new());
        restore(
            &blob,
            |stream| {
                component = read_all(stream);
                kResultOk
            },
            |stream| {
                synced = read_all(stream);
                kResultOk
            },
            |stream| {
                controller = read_all(stream);
                kResultOk
            },
        )
        .expect("taken");
        assert_eq!((component.as_slice(), synced.as_slice(), controller.as_slice()), (&b"the sound"[..], &b"the sound"[..], &b"the view"[..]), "the controller is brought in step with the component, and has its own");
    }

    #[test]
    fn a_state_with_no_controller_part_does_not_ask_the_controller_and_what_the_controller_says_is_not_held_against_it() {
        let blob = encode(b"the sound", b"");
        restore(&blob, |_| kResultOk, |_| kResultFalse, |_| unreachable!("there is nothing for the controller")).expect("the component took it");
        let blob = encode(b"the sound", b"the view");
        restore(&blob, |_| kResultOk, |_| kInvalidArgument, |_| kInvalidArgument).expect("the component is what counts");
    }

    #[test]
    fn a_state_that_the_component_does_not_take_or_a_blob_that_is_not_one_is_refused_with_nothing_asked_of_the_rest() {
        let blob = encode(b"the sound", b"the view");
        let error = restore(&blob, |_| kResultFalse, |_| unreachable!("not after a refusal"), |_| unreachable!("not after a refusal")).expect_err("refused");
        assert!(error.contains("did not take its state (result 1)"), "{error}");
        let error = restore(b"garbage", |_| unreachable!("no plugin is asked"), |_| unreachable!("none"), |_| unreachable!("none")).expect_err("refused");
        assert!(error.contains("not the state"), "{error}");
    }
}
