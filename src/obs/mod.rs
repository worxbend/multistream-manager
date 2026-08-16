//! Controlling OBS Studio.
//!
//! OBS is the other half of a stream. This program sets the title and the
//! category and reads the chat; OBS holds the scenes, the microphone and the
//! encoder, and is the thing that actually goes live. Having to leave a
//! terminal to press one button in a graphical window is exactly the sort of
//! interruption the rest of this program exists to avoid.
//!
//! The parts:
//!
//! * [`protocol`] — the obs-websocket 5 message format.
//! * [`auth`] — answering its authentication challenge.
//! * [`requests`] — every request this program sends.
//! * [`state`] — what OBS is currently doing, as far as this knows.
//! * [`event`] — what OBS says has changed.
//! * [`task`] — the connection itself: one task, commands in, events out.
//!
//! **OBS being absent is normal.** Plenty of people will never configure this,
//! and someone who has configured it will still start this program before
//! starting OBS about half the time. Nothing here may fail a start-up, block
//! a go-live, or put an error in front of somebody who never asked for OBS
//! control in the first place.

pub mod auth;
pub mod event;
pub mod protocol;
pub mod requests;
pub mod state;
pub mod task;
