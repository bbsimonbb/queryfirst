use qf_sqlfile::*;
use std::path::Path;

fn query(src: &str) -> QueryFile {
    parse_query_file(Path::new("q.psql"), &normalise(src)).unwrap()
}

fn lex(src: &str) -> LexedQuery {
    lex_params(&query(src)).unwrap()
}

fn names(l: &LexedQuery) -> Vec<&str> {
    l.uses.iter().map(|u| u.name.as_str()).collect()
}

#[test]
fn skips_casts() {
    let l = lex("SELECT :id::int");
    assert_eq!(names(&l), ["id"]);
    assert_eq!(l.db_statement, "SELECT $1::int");
}

#[test]
fn skips_strings_idents_dollar_comments() {
    let l = lex(concat!(
        "SELECT ':x', \"a:b\", $$ :y $$, $tag$ :z $tag$ -- :c\n",
        ", 1 /* :d */"
    ));
    assert!(l.uses.is_empty(), "{:?}", l.uses);
    assert!(l.param_order.is_empty());
}

#[test]
fn bang_and_question() {
    let l = lex("SELECT :a!, :b?");
    assert_eq!(names(&l), ["a", "b"]);
    assert_eq!(l.uses[0].marker, Marker::Bang);
    assert_eq!(l.uses[1].marker, Marker::Question);
    assert_eq!(l.db_statement, "SELECT $1, $2");
    assert!(!l.db_statement.contains('!') && !l.db_statement.contains('?'));
}

#[test]
fn repeated_param_one_placeholder() {
    let l = lex("SELECT * FROM t WHERE a = :id OR b = :id");
    assert_eq!(l.db_statement, "SELECT * FROM t WHERE a = $1 OR b = $1");
    assert_eq!(l.param_order, ["id"]);
    assert_eq!(l.uses.len(), 2);
}

const ADDRESS_PSQL: &str = "/* @name addressBelongsToAccount */\nSELECT count(*) > 0 as \"belongs\" \nFROM addresses A\ninner join customers C on A.\"customerId\" = C.id\nWHERE A.\"id\" = :addressId and C.\"accountId\" = :accountId\n;\n";

const ADDRESS_IR: &str = r#"{"usedParamSet":{"addressId":true,"accountId":true},"params":[{"name":"addressId","required":false,"transform":{"type":"scalar"},"locs":[{"a":114,"b":123}]},{"name":"accountId","required":false,"transform":{"type":"scalar"},"locs":[{"a":145,"b":154}]}],"statement":"SELECT count(*) > 0 as \"belongs\" \nFROM addresses A\ninner join customers C on A.\"customerId\" = C.id\nWHERE A.\"id\" = :addressId and C.\"accountId\" = :accountId"}"#;

#[test]
fn address_ir_exact() {
    let q = query(ADDRESS_PSQL);
    let l = lex_params(&q).unwrap();
    let ir = pgtyped_ir(&q, &l, &|_| false);
    assert_eq!(serde_json::to_string(&ir).unwrap(), ADDRESS_IR);
    assert_eq!(l.uses[0].loc, Loc { a: 114, b: 123 });
}

#[test]
fn utf16_locs() {
    let l = lex("SELECT 'é' , :p");
    assert_eq!(l.uses[0].loc, Loc { a: 13, b: 14 });
    // An emoji is 2 UTF-16 units.
    let l = lex("SELECT '😀' , :p");
    assert_eq!(l.uses[0].loc, Loc { a: 14, b: 15 });
    let l = lex("SELECT '😀' , :p!");
    assert_eq!(l.uses[0].loc, Loc { a: 14, b: 16 });
}

#[test]
fn sourcemap_error_position() {
    let src = "/* @name q */\nSELECT *\nFROM t\nWHERE a = :first\n  AND b = :second";
    let l = lex(src);
    assert_eq!(
        l.db_statement,
        "SELECT *\nFROM t\nWHERE a = $1\n  AND b = $2"
    );
    let pos = l.db_statement.find("$2").unwrap() as u32 + 1;
    // `:second` is on line 5, col 11.
    assert_eq!(l.map.db_char_pos_to_source(pos), (5, 11));
    // Without a header, on the first line.
    let l = lex("SELECT :a, :b");
    let pos = l.db_statement.find("$2").unwrap() as u32 + 1;
    assert_eq!(l.map.db_char_pos_to_source(pos), (1, 12));
    // Plain text maps one to one, and chars after a placeholder shift back.
    let l = lex("SELECT :abc, x");
    let pos = l.db_statement.find('x').unwrap() as u32 + 1;
    assert_eq!(l.map.db_char_pos_to_source(pos), (1, 14));
}

#[test]
fn block_comments_blanked_in_ir_statement() {
    let q = query("/* @name q */\nSELECT /* c */ :p");
    let l = lex_params(&q).unwrap();
    let ir = pgtyped_ir(&q, &l, &|_| true);
    assert_eq!(ir["statement"], "SELECT         :p");
    assert_eq!(ir["params"][0]["required"], true);
    assert_eq!(l.db_statement, "SELECT         $1");
}

#[test]
fn expansions_describe_with_one_element() {
    let src = "/* @name q\n  @param ids -> (...)\n  @param user -> (name, age!)\n  @param rows -> ((a, b)...)\n*/\nSELECT :x, :rows, :user, :ids";
    let q = query(src);
    let l = lex_params(&q).unwrap();
    // Declared params first, in declaration order, then the others in order of first use.
    assert_eq!(l.db_statement, "SELECT $6, ($4,$5), ($2,$3), ($1)");
    assert_eq!(
        l.param_order,
        ["ids", "user.name", "user.age", "rows.a", "rows.b", "x"]
    );
    let ir = pgtyped_ir(&q, &l, &|n| n == "x");
    let s = serde_json::to_string(&ir).unwrap();
    assert!(s.contains(r#"{"name":"user","required":false,"transform":{"type":"pick_tuple","keys":[{"name":"name","required":false},{"name":"age","required":true}]}"#), "{s}");
    assert!(s.contains(r#"{"type":"array_spread"}"#), "{s}");
    assert!(s.contains(r#"{"type":"pick_array_spread","keys":"#), "{s}");
    assert!(
        s.starts_with(r#"{"usedParamSet":{"x":true,"rows":true,"user":true,"ids":true}"#),
        "{s}"
    );
}

#[test]
fn unused_declared_param_has_no_required() {
    let q = query("/* @name q\n  @param ids -> (...)\n*/\nSELECT 1");
    let l = lex_params(&q).unwrap();
    let s = serde_json::to_string(&pgtyped_ir(&q, &l, &|_| true)).unwrap();
    assert_eq!(
        s,
        r#"{"usedParamSet":{},"params":[{"name":"ids","transform":{"type":"array_spread"},"locs":[]}],"statement":"SELECT 1"}"#
    );
}

#[test]
fn trailing_line_comment_keeps_newline() {
    let q = query("/* @name q */\nSELECT :p -- note\n;");
    let l = lex_params(&q).unwrap();
    assert_eq!(
        pgtyped_ir(&q, &l, &|_| false)["statement"],
        "SELECT :p -- note\n"
    );
}

#[test]
fn unterminated_string_is_qf001() {
    let q = query("/* @name q */\nSELECT 'abc");
    let e = lex_params(&q).unwrap_err();
    assert_eq!(e[0].code, "QF001");
    assert_eq!((e[0].line, e[0].col), (2, 8));
}

#[test]
fn conflicting_markers_is_qf001() {
    let e = lex_params(&query("SELECT :p!?")).unwrap_err();
    assert_eq!(e[0].code, "QF001");
}

#[test]
fn range_and_slices() {
    // `[1:2]` is not a param (pgtyped `range`), `[1:n]` is.
    let l = lex("SELECT a[1:2], b[1:n]");
    assert_eq!(names(&l), ["n"]);
}
