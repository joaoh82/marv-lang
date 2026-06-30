use marv_syntax::{format, parse};

#[test]
fn linear_interface_round_trips() {
    let src = "mod std.io\n\nlinear interface Conn {\n    fn close(conn: Conn) -> !\n}\n";
    parse(src).expect("linear interface parses");
    assert_eq!(format(src), src);
}

#[test]
fn linear_interface_must_be_non_generic() {
    let err =
        parse("mod m\n\nlinear interface Handle[T] {}\n").expect_err("generic linear interface");
    assert!(err.to_string().contains("non-generic capability resources"));
}
