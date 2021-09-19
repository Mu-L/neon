//! Rust wrappers for Node-API simple asynchronous operations
//!
//! Unlike `napi_async_work` which threads a single mutable pointer to a data
//! struct to both the `execute` and `complete` callbacks, the wrapper follows
//! a more idiomatic Rust ownership pattern by passing the output of `execute`
//! into the input of `complete`.
//!
//! https://nodejs.org/api/n-api.html#n_api_simple_asynchronous_operations

use std::ffi::c_void;
use std::mem;
use std::ptr;

use crate::napi::bindings as napi;
use crate::raw::Env;

type Execute<T, O> = fn(input: T) -> O;
type Complete<O> = fn(env: Env, output: O);

/// Schedule work to execute on the libuv thread pool
///
/// # Safety
/// * `env` must be a valid `napi_env` for the current thread
pub unsafe fn schedule<T, O>(env: Env, input: T, execute: Execute<T, O>, complete: Complete<O>)
where
    T: Send + 'static,
    O: Send + 'static,
{
    let mut data = Box::new(Data {
        state: State::Input(input),
        execute,
        complete,
        // Work is initialized as a null pointer, but set by `create_async_work`
        // `data` must not be used until this value has been set.
        work: ptr::null_mut(),
    });

    // Store a pointer to `work` before ownership is transferred to `Box::into_raw`
    let work = &mut data.work as *mut _;

    // Create the `async_work`
    assert_eq!(
        napi::create_async_work(
            env,
            ptr::null_mut(),
            super::string(env, "neon_async_work"),
            Some(call_execute::<T, O>),
            Some(call_complete::<T, O>),
            Box::into_raw(data).cast(),
            work,
        ),
        napi::Status::Ok,
    );

    // Queue the work
    match napi::queue_async_work(env, *work) {
        napi::Status::Ok => {}
        status => {
            // If queueing failed, delete the work to prevent a leak
            napi::delete_async_work(env, *work);
            assert_eq!(status, napi::Status::Ok);
        }
    }
}

/// A pointer to data is passed to the `execute` and `complete` callbacks
struct Data<T, O> {
    state: State<T, O>,
    execute: Execute<T, O>,
    complete: Complete<O>,
    work: napi::AsyncWork,
}

/// State of the task that is transitioned by `execute` and `complete`
enum State<T, O> {
    /// Initial data input passed to `execute`
    Input(T),
    /// Transient state while `execute` is running
    Executing,
    /// Return data of `execute` passed to `complete`
    Output(O),
}

impl<T, O> State<T, O> {
    /// Return the input if `State::Input`, replacing with `State::Executing`
    fn take_execute_input(&mut self) -> Option<T> {
        match mem::replace(self, Self::Executing) {
            Self::Input(input) => Some(input),
            _ => None,
        }
    }

    /// Return the output if `State::Output`, replacing with `State::Executing`
    fn into_output(self) -> Option<O> {
        match self {
            Self::Output(output) => Some(output),
            _ => None,
        }
    }
}

/// Callback executed on the libuv thread pool
///
/// # Safety
/// * `Env` should not be used because it could attempt to call JavaScript
/// * `data` is expected to be a pointer to `Data<T, O>`
unsafe extern "C" fn call_execute<T, O>(_: Env, data: *mut c_void) {
    let data = &mut *data.cast::<Data<T, O>>();
    // `unwrap` is ok because `call_execute` should be called exactly once
    // after initialization
    let input = data.state.take_execute_input().unwrap();
    let output = (data.execute)(input);

    data.state = State::Output(output);
}

/// Callback executed on the JavaScript main thread
///
/// # Safety
/// * `data` is expected to be a pointer to `Data<T, O>`
unsafe extern "C" fn call_complete<T, O>(env: Env, status: napi::Status, data: *mut c_void) {
    let Data {
        state,
        complete,
        work,
        ..
    } = *Box::<Data<T, O>>::from_raw(data.cast());

    napi::delete_async_work(env, work);

    match status {
        // `unwrap` is okay because `call_complete` should be called exactly once
        // if and only if `call_execute` has completed successfully
        napi::Status::Ok => complete(env, state.into_output().unwrap()),
        napi::Status::Cancelled => {}
        _ => assert_eq!(status, napi::Status::Ok),
    }
}