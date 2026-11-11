//! The shared library (`libunitn_android.so`) that Android loads for
//! `MainActivity`. All of the app lives in the `unitn` crate; this adds the
//! calls into the Java side (`java/net/bokors/unitn`): the playback service
//! with its notification, opening the browser, and the permission questions.
#![cfg(target_os = "android")]

use std::ffi::c_void;

use jni::objects::{JClass, JObject, JValue};
use jni::sys::{jint, jlong};
use jni::{JNIEnv, JavaVM, NativeMethod};
use unitn::{NowPlaying, Platform};

#[no_mangle]
fn android_main(app: unitn::AndroidApp) {
    register_natives();
    unitn::android_main(app, Box::new(Java));
}

/// Runs `f` with a JNI environment and the activity object.
fn with_activity<R>(what: &str, f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
    let context = ndk_context::android_context();
    let vm = match unsafe { JavaVM::from_raw(context.vm().cast()) } {
        Ok(vm) => vm,
        Err(e) => {
            unitn::logcat(&format!("android: {what}: no Java VM: {e}"));
            return None;
        }
    };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(env) => env,
        Err(e) => {
            unitn::logcat(&format!("android: {what}: cannot attach to the Java VM: {e}"));
            return None;
        }
    };
    // The activity is a global reference owned by the Android glue.
    let activity = unsafe { JObject::from_raw(context.context().cast()) };
    // A local frame frees the Java strings made during the call.
    let result = env.with_local_frame(16, |env| f(env, &activity));
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    match result {
        Ok(value) => Some(value),
        Err(e) => {
            unitn::logcat(&format!("android: {what} failed: {e}"));
            None
        }
    }
}

/// `MainActivity.nativeCommand(int, long)`: events from the Java side.
extern "system" fn native_command(_env: JNIEnv, _class: JClass, code: jint, arg: jlong) {
    unitn::on_java_event(code, arg);
}

fn register_natives() {
    with_activity("registering the native methods", |env, activity| {
        let class = env.get_object_class(activity)?;
        let methods = [NativeMethod {
            name: "nativeCommand".into(),
            sig: "(IJ)V".into(),
            fn_ptr: native_command as *mut c_void,
        }];
        env.register_native_methods(&class, &methods)
    });
}

struct Java;

impl Platform for Java {
    fn playback(&self, now: Option<&NowPlaying>) {
        with_activity("updating the playback notification", |env, activity| {
            let Some(now) = now else {
                env.call_method(activity, "stopPlayback", "()V", &[])?;
                return Ok(());
            };
            let title = env.new_string(&now.title)?;
            let artist = env.new_string(&now.artist)?;
            let album = env.new_string(&now.album)?;
            let position = (now.position * 1000.0) as i64;
            let duration = now.duration.map_or(-1, |d| (d * 1000.0) as i64);
            env.call_method(
                activity,
                "updatePlayback",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJZ)V",
                &[
                    JValue::Object(&title),
                    JValue::Object(&artist),
                    JValue::Object(&album),
                    JValue::Long(position),
                    JValue::Long(duration),
                    JValue::Bool(now.playing as u8),
                ],
            )?;
            Ok(())
        });
    }

    fn open_url(&self, url: &str) -> bool {
        with_activity("opening the browser", |env, activity| {
            let url = env.new_string(url)?;
            env.call_method(activity, "openUrl", "(Ljava/lang/String;)Z", &[JValue::Object(&url)])?.z()
        })
        .unwrap_or(false)
    }

    fn request_permissions(&self) {
        with_activity("asking for permissions", |env, activity| {
            env.call_method(activity, "requestAppPermissions", "()V", &[])?;
            Ok(())
        });
    }
}
