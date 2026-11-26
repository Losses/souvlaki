#![cfg(target_os = "macos")]

//! Whether the objects the macOS backend creates for metadata and artwork are released again.
//!
//! `MPMediaItemArtwork` objects are counted as they are allocated and deallocated. The backend
//! is the only one that makes them in this process, and each holds its image through its
//! request handler, so an artwork that is never deallocated keeps its image as well.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::geometry::CGSize;
use objc::rc::{autoreleasepool, StrongPtr};
use objc::runtime::{
    class_addMethod, class_getInstanceMethod, method_getImplementation, object_getClass, Class,
    Imp, Object, Sel, NO,
};
use objc::{class, msg_send, sel, sel_impl};
use souvlaki::{MediaControls, MediaMetadata, PlatformConfig};

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATED: AtomicUsize = AtomicUsize::new(0);
/// The implementations the counting ones call on: `+[NSObject alloc]` and `-[NSObject dealloc]`.
static ALLOC: AtomicUsize = AtomicUsize::new(0);
static DEALLOC: AtomicUsize = AtomicUsize::new(0);

extern "C" fn counted_alloc(class: &Class, selector: Sel) -> *mut Object {
    ALLOCATED.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the stored implementation is `+[NSObject alloc]`, which has this signature.
    let alloc: extern "C" fn(&Class, Sel) -> *mut Object =
        unsafe { std::mem::transmute(ALLOC.load(Ordering::SeqCst)) };
    alloc(class, selector)
}

extern "C" fn counted_dealloc(object: &mut Object, selector: Sel) {
    DEALLOCATED.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the stored implementation is the `dealloc` the class had, which has this signature.
    let dealloc: extern "C" fn(&mut Object, Sel) =
        unsafe { std::mem::transmute(DEALLOC.load(Ordering::SeqCst)) };
    dealloc(object, selector)
}

/// Count the `MPMediaItemArtwork` objects from now on, by overriding `alloc` and `dealloc` in
/// that class with methods that count and then do what the class did before.
fn count_artworks() {
    let artwork = class!(MPMediaItemArtwork);
    // SAFETY: the methods exist on every `NSObject` subclass; the overrides are added to
    // `MPMediaItemArtwork` alone (its metaclass for `alloc`), with the type encodings of the
    // methods they stand in for.
    unsafe {
        let metaclass = object_getClass(artwork as *const Class as *const Object);
        let alloc = class_getInstanceMethod(metaclass, sel!(alloc));
        ALLOC.store(method_getImplementation(alloc) as usize, Ordering::SeqCst);
        let counted: Imp =
            std::mem::transmute(counted_alloc as extern "C" fn(&Class, Sel) -> *mut Object);
        assert!(
            class_addMethod(
                metaclass as *mut Class,
                sel!(alloc),
                counted,
                b"@@:\0".as_ptr().cast()
            ) != NO,
            "MPMediaItemArtwork has an alloc of its own"
        );

        let dealloc = class_getInstanceMethod(artwork, sel!(dealloc));
        DEALLOC.store(method_getImplementation(dealloc) as usize, Ordering::SeqCst);
        let counted: Imp = std::mem::transmute(counted_dealloc as extern "C" fn(&mut Object, Sel));
        assert!(
            class_addMethod(
                artwork as *const Class as *mut Class,
                sel!(dealloc),
                counted,
                b"v@:\0".as_ptr().cast()
            ) != NO,
            "MPMediaItemArtwork has a dealloc of its own"
        );
    }
}

#[link(name = "MediaPlayer", kind = "framework")]
extern "C" {
    static MPMediaItemPropertyTitle: *mut Object;
    static MPMediaItemPropertyArtwork: *mut Object;
}

/// What the Now Playing info has for `key`, or null.
unsafe fn shown(key: *mut Object) -> *mut Object {
    let center: *mut Object = msg_send![class!(MPNowPlayingInfoCenter), defaultCenter];
    let info: *mut Object = msg_send![center, nowPlayingInfo];
    msg_send![info, objectForKey: key]
}

/// A one pixel PNG.
const PNG: [u8; 67] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "{}: {} artworks allocated, {} deallocated",
            what,
            ALLOCATED.load(Ordering::SeqCst),
            DEALLOCATED.load(Ordering::SeqCst)
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn artwork_of_replaced_and_stale_updates_is_released() {
    const UPDATES: usize = 20;
    count_artworks();
    let cover = std::env::temp_dir().join(format!("souvlaki-cover-{}.png", std::process::id()));
    fs::write(&cover, PNG).unwrap();
    let cover_url = format!("file://{}", cover.display());
    let mut controls = MediaControls::new(PlatformConfig {
        dbus_name: "souvlaki_test",
        display_name: "Souvlaki test",
        hwnd: None,
    })
    .unwrap();

    // The calls are made as an application makes them from a thread of its own, which has no
    // autorelease pool. One update right after the other: the artwork of most of them is stale
    // before it is made.
    for update in 0..UPDATES {
        let title = format!("A title long enough to be an object, number {}", update);
        controls
            .set_metadata(MediaMetadata {
                title: Some(&title),
                artist: Some("An artist with a name of some length"),
                album: Some("An album with a name of some length"),
                cover_url: Some(&cover_url),
                duration: Some(Duration::from_secs(200)),
            })
            .unwrap();
    }
    wait_for("an artwork for every update", || {
        ALLOCATED.load(Ordering::SeqCst) == UPDATES
    });

    // What is shown now: the title and the artwork of the last update, and the artwork's image.
    wait_for("the artwork of the last update shown", || {
        autoreleasepool(|| unsafe { !shown(MPMediaItemPropertyArtwork).is_null() })
    });
    let (title, image) = autoreleasepool(|| unsafe {
        let artwork = shown(MPMediaItemPropertyArtwork);
        let image: *mut Object = msg_send![artwork, imageWithSize: CGSize::new(1.0, 1.0)];
        assert!(!image.is_null(), "the artwork has no image");
        (
            StrongPtr::retain(shown(MPMediaItemPropertyTitle)).weak(),
            StrongPtr::retain(image).weak(),
        )
    });

    // An update without a cover: no artwork is shown any more.
    controls
        .set_metadata(MediaMetadata {
            title: Some("The last title, which has no cover"),
            ..Default::default()
        })
        .unwrap();
    wait_for("every artwork released", || {
        DEALLOCATED.load(Ordering::SeqCst) == UPDATES
    });
    wait_for("the replaced title and the image released", || {
        autoreleasepool(|| title.load().is_null() && image.load().is_null())
    });
    fs::remove_file(cover).unwrap();
}
