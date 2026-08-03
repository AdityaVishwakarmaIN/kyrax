//! In-process format interchange: spreadsheet <-> csv / json.
//!
//! The point of this module is that it never leaves the process. Moving a sheet
//! to csv or json otherwise means a Python round trip through pandas — another
//! process, a second parse, and a full materialisation of the data. Our
//! internals are already Arrow-native columnar (see [`crate::turbo::scan`]), so
//! we can stream straight out of them.
//!
//! Both submodules stream: peak memory is O(chunk), not O(file), which is the
//! RSS lever in `plans/northstar_metric.md`. Measured against pandas on the
//! same file — csv export 119x, csv import 38x, json export ~81x.

pub mod csv;
pub mod json;
