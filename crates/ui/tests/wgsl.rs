//! Offline WGSL validation: parse + validate the render shader with naga (no GPU needed).
#[test]
fn render_shader_is_valid() {
    let source = berth_ui::render::shader_source();
    let module = naga::front::wgsl::parse_str(source).expect("shader must parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("shader must pass validation");
}

#[test]
fn paren_construct_parses() {
    // Regression guard: naga 30 constructs structs parenthesized (declarational order),
    // and does NOT implement WGSL brace literals.
    parse_ok(
        "paren literal",
        r#"struct S { a: f32 };
@fragment fn f() -> @location(0) vec4<f32> { let s = S(1.0); return vec4<f32>(s.a); }"#,
    );
    assert!(naga::front::wgsl::parse_str(
        "struct S { a: f32 };\n@fragment fn f() -> @location(0) vec4<f32> { let s = S { a: 1.0 }; return vec4<f32>(s.a); }",
    )
    .is_err());
}

fn parse_ok(name: &str, src: &str) {
    if let Err(e) = naga::front::wgsl::parse_str(src) {
        panic!("{name}: {e}");
    }
}
