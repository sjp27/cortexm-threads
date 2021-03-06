//!
//! A simple library for context-switching on ARM Cortex-M ( 0, 0+, 3, 4, 4F ) micro-processors
//!
//! Supports pre-emptive, priority based switching
//!
//! This project is meant for learning and should be used only at the user's risk. For practical and mature
//! rust alternatives, see [Awesome Embedded Rust](https://github.com/rust-embedded/awesome-embedded-rust)
//!
//! Example:
//!
//! See [example crate on github](https://github.com/n-k/cortexm-threads/tree/master/example_crates/qemu-m4)
//! ```
//! #![no_std]
//! #![no_main]
//!
//! extern crate panic_semihosting;
//! use cortex_m::peripheral::syst::SystClkSource;
//! use cortex_m_rt::{entry, exception};
//! use cortex_m_semihosting::{hprintln};
//! use cortexm_threads::{init, create_thread, create_thread_with_config, sleep};
//!
//! #[entry]
//! fn main() -> ! {
//!     let cp = cortex_m::Peripherals::take().unwrap();
//!     let mut syst = cp.SYST;
//!     syst.set_clock_source(SystClkSource::Core);
//!     syst.set_reload(80_000);
//!     syst.enable_counter();
//!     syst.enable_interrupt();
//!
//! 	let mut stack1 = [0xDEADBEEF; 512];
//!     let mut stack2 = [0xDEADBEEF; 512];
//!     let _ = create_thread(
//!         &mut stack1,
//!         || {
//!             loop {
//!                 let _ = hprintln!("in task 1 !!");
//!                 sleep(50); // sleep for 50 ticks
//!             }
//!         });
//!     let _ = create_thread_with_config(
//!         &mut stack2,
//!         || {
//!             loop {
//!                 let _ = hprintln!("in task 2 !!");
//!                 sleep(30); // sleep for 30 ticks
//!             }
//!         },
//!         0x01, // priority, higher numeric value means higher priority
//!         true  // privileged thread
//! 		);
//!     init();
//! }
//! ```
#![no_std]

use core::ptr;

/// Returned by create_thread or create_thread_with_config as Err(ERR_TOO_MANY_THREADS)
/// if creating a thread will cause more than 32 threads to exist (inclusing the idle thread)
/// created by this library
pub static ERR_TOO_MANY_THREADS: u8 = 0x01;
/// Returned by create_thread or create_thread_with_config as Err(ERR_STACK_TOO_SMALL)
/// if array to be used as stack area is too small. Smallest size is 32 u32's
pub static ERR_STACK_TOO_SMALL: u8 = 0x02;
/// Returned by create_thread or create_thread_with_config as Err(ERR_NO_CREATE_PRIV)
/// if called from an unprivileged thread
pub static ERR_NO_CREATE_PRIV: u8 = 0x03;

/// Context switching and threads' state
#[repr(C)]
struct ThreadsState {
    // start fields used in assembly, do not change their order
    curr: usize,
    next: usize,
    // end fields used in assembly
    inited: bool,
    idx: usize,
    add_idx: usize,
    threads: [ThreadControlBlock; 32],
}

/// Thread status
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ThreadStatus {
    Idle,
    Sleeping,
}

/// A single thread's state
#[repr(C)]
#[derive(Clone, Copy)]
struct ThreadControlBlock {
    // start fields used in assembly, do not reorder them
    /// current stack pointer of this thread
    sp: u32,
    privileged: u32, // make it a word, assembly is easier. FIXME
    // end fields used in assembly
    priority: u8,
    status: ThreadStatus,
    sleep_ticks: u32,
}

// GLOBALS:
#[no_mangle]
static mut __CORTEXM_THREADS_GLOBAL_PTR: u32 = 0;
static mut __CORTEXM_THREADS_GLOBAL: ThreadsState = ThreadsState {
    curr: 0,
    next: 0,
    inited: false,
    idx: 0,
    add_idx: 1,
    threads: [ThreadControlBlock {
        sp: 0,
        status: ThreadStatus::Idle,
        priority: 0,
        privileged: 0,
        sleep_ticks: 0,
    }; 32],
};
// end GLOBALS

// functions defined in assembly
extern "C" {
    fn __CORTEXM_THREADS_cpsid();
    fn __CORTEXM_THREADS_cpsie();
    fn __CORTEXM_THREADS_wfe();
}

/// Initialize the switcher system
pub fn init() -> ! {
    unsafe {
        __CORTEXM_THREADS_cpsid();
        let ptr: usize = core::intrinsics::transmute(&__CORTEXM_THREADS_GLOBAL);
        __CORTEXM_THREADS_GLOBAL_PTR = ptr as u32;
        __CORTEXM_THREADS_cpsie();
        let mut idle_stack = [0xDEADBEEF; 64];
        match create_tcb(
            &mut idle_stack,
            || loop {
                __CORTEXM_THREADS_wfe();
            },
            0xff,
            false,
        ) {
            Ok(tcb) => {
                insert_tcb(0, tcb);
            }
            _ => panic!("Could not create idle thread"),
        }
        __CORTEXM_THREADS_GLOBAL.inited = true;
        SysTick();
        loop {
            __CORTEXM_THREADS_wfe();
        }
    }
}

/// Create a thread with default configuration (lowest priority, unprivileged).
///
/// # Arguments
/// * stack: mut array of u32's to be used as stack area
/// * handler_fn: function to execute in created thread
///
/// # Example
/// ```
/// let mut stack1 = [0xDEADBEEF; 512];
/// let _ = create_thread(
///     &mut stack1,
///     || {
///         loop {
///             let _ = hprintln!("in task 1 !!");
///             sleep(50);
///         }
///     });
///```
pub fn create_thread(stack: &mut [u32], handler_fn: fn() -> !) -> Result<(), u8> {
    create_thread_with_config(stack, handler_fn, 0x00, false)
}

/// Create a thread with explicit configuration
/// # Arguments
/// * stack: mut array of u32's to be used as stack area
/// * handler_fn: function to execute in created thread
/// * priority: higher numeric value means higher priority
/// * privileged: run thread in privileged mode
///
/// # Example
/// ```
/// let mut stack1 = [0xDEADBEEF; 512];
/// let _ = create_thread_with_config(
///     &mut stack1,
///     || {
///         loop {
///             let _ = hprintln!("in task 1 !!");
///             sleep(50);
///         }
///     },
///     0xff, // priority, this is the maximum, higher number means higher priority
///     true // this thread will be run in privileged mode
///     );
///```
/// FIXME: take stack memory as a vec (arrayvec?, smallvec?) instead of &[]
pub fn create_thread_with_config(
    stack: &mut [u32],
    handler_fn: fn() -> !,
    priority: u8,
    priviliged: bool,
) -> Result<(), u8> {
    unsafe {
        __CORTEXM_THREADS_cpsid();
        let handler = &mut __CORTEXM_THREADS_GLOBAL;
        if handler.add_idx >= handler.threads.len() {
            return Err(ERR_TOO_MANY_THREADS);
        }
        if handler.inited && handler.threads[handler.idx].privileged == 0 {
            return Err(ERR_NO_CREATE_PRIV);
        }
        match create_tcb(stack, handler_fn, priority, priviliged) {
            Ok(tcb) => {
                insert_tcb(handler.add_idx, tcb);
                handler.add_idx = handler.add_idx + 1;
            }
            Err(e) => {
                __CORTEXM_THREADS_cpsie();
                return Err(e);
            }
        }
        __CORTEXM_THREADS_cpsie();
        Ok(())
    }
}

/// Handle a tick event. Typically, this would be called as SysTick handler, but can be
/// called anytime. Call from thread handler code to yield and switch context.
///
/// * updates sleep_ticks field in sleeping threads, decreses by 1
/// * if a sleeping thread has sleep_ticks == 0, wake it, i.e., change status to idle
/// * find next thread to schedule
/// * if context switch is required, will pend the PendSV exception, which will do the actual thread switching
#[no_mangle]
pub extern "C" fn SysTick() {
    unsafe {
        __CORTEXM_THREADS_cpsid();
    }
    let handler = unsafe { &mut __CORTEXM_THREADS_GLOBAL };
    if handler.inited {
        if handler.curr == handler.next {
            // schedule a thread to be run
            handler.idx = get_next_thread_idx();
            unsafe {
                handler.next = core::intrinsics::transmute(&handler.threads[handler.idx]);
            }
        }
        if handler.curr != handler.next {
            unsafe {
                let pend = ptr::read_volatile(0xE000ED04 as *const u32);
                ptr::write_volatile(0xE000ED04 as *mut u32, pend | 1 << 28);
            }
        }
    }
    unsafe {
        __CORTEXM_THREADS_cpsie();
    }
}

/// Get id of current thread
pub fn get_thread_id() -> usize {
    let handler = unsafe { &mut __CORTEXM_THREADS_GLOBAL };
    handler.idx
}

/// Make current thread sleep for `ticks` ticks. Current thread will be put in `Sleeping`
/// state and another thread will be scheduled immediately. Current thread will not be considered
/// for scheduling until `tick()` is called at least `tick` times.
///
/// # Example
/// ```
/// let mut stack1 = [0xDEADBEEF; 512];
/// let _ = create_thread(
///     &mut stack1,
///     || {
///         loop {
///             let _ = hprintln!("in task 1 !!");
///             sleep(50);
///         }
///     });
/// ```
pub fn sleep(ticks: u32) {
    let handler = unsafe { &mut __CORTEXM_THREADS_GLOBAL };
    if handler.idx > 0 {
        handler.threads[handler.idx].status = ThreadStatus::Sleeping;
        handler.threads[handler.idx].sleep_ticks = ticks;
        // schedule another thread
        SysTick();
    }
}

fn get_next_thread_idx() -> usize {
    let handler = unsafe { &mut __CORTEXM_THREADS_GLOBAL };
    if handler.add_idx <= 1 {
        // no user threads, schedule idle thread
        return 0;
    }
    // user threads exist
    // update sleeping threads
    for i in 1..handler.add_idx {
        if handler.threads[i].status == ThreadStatus::Sleeping {
            if handler.threads[i].sleep_ticks > 0 {
                handler.threads[i].sleep_ticks = handler.threads[i].sleep_ticks - 1;
            } else {
                handler.threads[i].status = ThreadStatus::Idle;
            }
        }
    }
    match handler
        .threads
        .into_iter()
        .enumerate()
        .filter(|&(idx, x)| idx > 0 && idx < handler.add_idx && x.status != ThreadStatus::Sleeping)
        .max_by(|&(_, a), &(_, b)| a.priority.cmp(&b.priority))
    {
        Some((idx, _)) => idx,
        _ => 0,
    }
}

fn create_tcb(
    stack: &mut [u32],
    handler: fn() -> !,
    priority: u8,
    priviliged: bool,
) -> Result<ThreadControlBlock, u8> {
    if stack.len() < 32 {
        return Err(ERR_STACK_TOO_SMALL);
    }
    let idx = stack.len() - 1;
    stack[idx] = 1 << 24; // xPSR
    let pc: usize = unsafe { core::intrinsics::transmute(handler as *const fn()) };
    stack[idx - 1] = pc as u32; // PC
    stack[idx - 2] = 0xFFFFFFFD; // LR
    stack[idx - 3] = 0xCCCCCCCC; // R12
    stack[idx - 4] = 0x33333333; // R3
    stack[idx - 5] = 0x22222222; // R2
    stack[idx - 6] = 0x11111111; // R1
    stack[idx - 7] = 0x00000000; // R0
                                 // aditional regs
    stack[idx - 08] = 0x77777777; // R7
    stack[idx - 09] = 0x66666666; // R6
    stack[idx - 10] = 0x55555555; // R5
    stack[idx - 11] = 0x44444444; // R4
    stack[idx - 12] = 0xBBBBBBBB; // R11
    stack[idx - 13] = 0xAAAAAAAA; // R10
    stack[idx - 14] = 0x99999999; // R9
    stack[idx - 15] = 0x88888888; // R8
    unsafe {
        let sp: usize = core::intrinsics::transmute(&stack[stack.len() - 16]);
        let tcb = ThreadControlBlock {
            sp: sp as u32,
            priority: priority,
            privileged: if priviliged { 0x1 } else { 0x0 },
            status: ThreadStatus::Idle,
            sleep_ticks: 0,
        };
        Ok(tcb)
    }
}

fn insert_tcb(idx: usize, tcb: ThreadControlBlock) {
    unsafe {
        let handler = &mut __CORTEXM_THREADS_GLOBAL;
        handler.threads[idx] = tcb;
    }
}
