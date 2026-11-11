//! The shared library (`libunitn_android.so`) that Android's NativeActivity
//! loads. All of the app lives in the `unitn` crate.
#![cfg(target_os = "android")]

#[no_mangle]
fn android_main(app: unitn::AndroidApp) {
    unitn::android_main(app);
}
