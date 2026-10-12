//! Boot-scoped adb policy. Never loaded on a non-debug boot.
pub const POLICY: &str = "\
allow adbd adbd process setcurrent
allow adbd egysk process dyntransition
allow init adbd process noatsecure
allow init adbd process2 nosuid_transition
allow adbd egysk_file dir { search getattr }
allow adbd egysk_file file { read open getattr execute execute_no_trans map }
allow { system_server untrusted_app_all shell } egysk unix_stream_socket { connectto read write getattr getopt setopt shutdown }
allow { system_server untrusted_app_all shell } egysk fd use
allow { system_server untrusted_app_all shell } egysk fifo_file { read write getattr open }
allow { system_server untrusted_app_all shell } egysk binder { call transfer }
";
