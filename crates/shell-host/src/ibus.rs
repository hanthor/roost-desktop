//! Shared IBus wire values and bus discovery for the input bridge and GTK panel.

use gio::glib;
use gio::prelude::*;

/// The string inside a serialized IBusText (`v` holding `(sa{sv}sv)`).
pub fn text(value: &glib::Variant) -> Option<String> {
    // as_variant calls g_variant_get_variant directly; its Option result does
    // not protect ordinary serialized tuples from GLib's type assertion.
    let mut inner = value.clone();
    while inner.is_type(glib::VariantTy::VARIANT) {
        inner = inner.as_variant()?;
    }
    inner.try_child_value(2)?.str().map(str::to_owned)
}

/// A serialized IBusText with no attributes, as libibus sends one:
/// `v` holding `("IBusText", a{sv} {}, s text, v ("IBusAttrList",
/// a{sv} {}, av []))`.
pub fn text_variant(text: &str) -> glib::Variant {
    let no_props = || glib::VariantDict::new(None).end();
    let attrs = glib::Variant::tuple_from_iter([
        "IBusAttrList".to_variant(),
        no_props(),
        glib::Variant::array_from_iter_with_type(
            glib::VariantTy::VARIANT,
            std::iter::empty::<glib::Variant>(),
        ),
    ]);
    glib::Variant::from_variant(&glib::Variant::tuple_from_iter([
        "IBusText".to_variant(),
        no_props(),
        text.to_variant(),
        glib::Variant::from_variant(&attrs),
    ]))
}

/// `IBUS_ADDRESS`, else what `ibus address` reports for this display.
pub fn address() -> Option<String> {
    if let Some(address) = std::env::var("IBUS_ADDRESS").ok().filter(|a| !a.is_empty()) {
        return Some(address);
    }
    let out = std::process::Command::new("ibus")
        .arg("address")
        .output()
        .ok()?;
    let address = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!address.is_empty() && address != "(null)").then_some(address)
}

/// Enable the existing opt-in IBus diagnostics in either process.
pub fn debug() -> bool {
    std::env::var_os("TUNA_IBUS_DEBUG").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibus_text_encoding_matches_the_libibus_wire_shape() {
        let packed = text_variant("你好");
        assert_eq!(packed.type_().as_str(), "v");
        let inner = packed.as_variant().unwrap();
        assert_eq!(inner.type_().as_str(), "(sa{sv}sv)");
        assert_eq!(inner.child_value(0).str(), Some("IBusText"));
        let attrs = inner.child_value(3).as_variant().unwrap();
        assert_eq!(attrs.type_().as_str(), "(sa{sv}av)");
        assert_eq!(attrs.child_value(0).str(), Some("IBusAttrList"));
        assert_eq!(attrs.child_value(2).n_children(), 0);
        assert_eq!(text(&packed).as_deref(), Some("你好"));
    }

    #[test]
    fn raw_and_nested_ibus_text_decode_without_native_type_assertions() {
        let boxed = text_variant("你好");
        for value in [
            boxed.as_variant().unwrap(),
            boxed.clone(),
            glib::Variant::from_variant(&boxed),
        ] {
            assert_eq!(text(&value).as_deref(), Some("你好"));
        }
        for value in ["not a container".to_variant(), 42u32.to_variant()] {
            assert_eq!(text(&value), None);
        }
    }
}
