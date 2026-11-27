use super::sanitize_html;

#[test]
fn sanitize_html_redacts_all_password_value_variants_and_ignores_non_input_attrs() {
    let cases = [
        (
            r#"<input data-value="foo" type="password" value="hunter2">"#,
            "hunter2",
            Some(r#"data-value="foo""#),
        ),
        (
            r#"<input value="hunter2" type="password">"#,
            "hunter2",
            None,
        ),
        (
            r#"<input type = 'password' value = 'hunter2'>"#,
            "hunter2",
            None,
        ),
        (r#"<input type=password value=hunter2>"#, "hunter2", None),
        (
            r#"<input type=password value=hunter2/>"#,
            "hunter2",
            Some("/>"),
        ),
        (
            r#"<INPUT TYPE="PASSWORD" VALUE="a" value='b' value=secret3>"#,
            "secret3",
            None,
        ),
        (
            r#"<input title="a>b" type="password" value="hunter2">"#,
            "hunter2",
            Some(r#"title="a>b""#),
        ),
        (
            r#"<!-- don't --><input type="password" value="hunter2">"#,
            "hunter2",
            Some("<!-- don't -->"),
        ),
    ];
    for (html, secret, must_keep) in cases {
        let out = sanitize_html(html);
        assert!(!out.contains(secret), "leaked `{secret}` in `{out}`");
        if let Some(keep) = must_keep {
            assert!(out.contains(keep), "expected `{keep}` in `{out}`");
        }
    }
    let non_input = r#"<div data-x="<input type='password' value='keep'>"></div>"#;
    assert_eq!(sanitize_html(non_input), non_input);
    let text_input = r#"<input type="text" value="keep">"#;
    assert_eq!(sanitize_html(text_input), text_input);
}

#[test]
fn sanitize_html_redacts_password_inputs_after_unclosed_lt_and_script_nesting() {
    let cases = [
        (
            r#"<scr<script>ipt>nested_js</script><input type="password" value="after_script_pw">"#,
            "after_script_pw",
        ),
        (
            r#"1 < 2 <input type="password" value="lt_text_pw">"#,
            "lt_text_pw",
        ),
        (
            r#"1 < don't <input type="password" value="lt_quote_pw">"#,
            "lt_quote_pw",
        ),
    ];
    for (html, secret) in cases {
        let out = sanitize_html(html);
        assert!(
            !out.contains(secret),
            "leaked `{secret}` in `{out}` for `{html}`"
        );
        assert!(
            out.contains(r#"value="""#),
            "expected redacted value=\"\" in `{out}`"
        );
    }
}

#[test]
fn sanitize_html_redacts_password_inputs_after_lt_eq_quote_sequences() {
    let cases = [
        (
            r#"1 < 2 = "unclosed <input type="password" value="pw_eq_quote">"#,
            "pw_eq_quote",
        ),
        (
            r#"a < b="unclosed <input type="password" value="pw_eq_quote2">"#,
            "pw_eq_quote2",
        ),
        (
            r#"价格 < 100="说明 <input type="password" value="pw_eq_utf8">"#,
            "pw_eq_utf8",
        ),
    ];
    for (html, secret) in cases {
        let out = sanitize_html(html);
        assert!(
            !out.contains(secret),
            "leaked `{secret}` in `{out}` for `{html}`"
        );
        assert!(
            out.contains(r#"value="""#),
            "expected redacted value=\"\" in `{out}`"
        );
    }
    let quoted_attr = r#"<div data-x="<input type=password value=pw>"></div>"#;
    assert_eq!(sanitize_html(quoted_attr), quoted_attr);
}

#[test]
fn sanitize_html_closes_challenger_bypasses() {
    let cases = [
        (
            r#"<p>a<b="1 <input type=password value=pw_alpha_lt></p>"#,
            "pw_alpha_lt",
        ),
        (
            r#"a<b="unclosed <input type="password" value="pw_alpha_lt2">"#,
            "pw_alpha_lt2",
        ),
        (
            r#"1</2="unclosed <input type=password value=pw_slash_lt>"#,
            "pw_slash_lt",
        ),
        (
            r#"a<?b="unclosed <input type=password value=pw_q_lt>"#,
            "pw_q_lt",
        ),
        (
            r#"a<!b="unclosed <input type=password value=pw_bang_lt>"#,
            "pw_bang_lt",
        ),
        (
            r#"<!--><input type="password" value="pw_abrupt1">"#,
            "pw_abrupt1",
        ),
        (
            r#"<!---><input type="password" value="pw_abrupt2">"#,
            "pw_abrupt2",
        ),
        (
            r#"<!-- comment --!><input type="password" value="pw_bang_comment">"#,
            "pw_bang_comment",
        ),
    ];
    for (html, secret) in cases {
        let out = sanitize_html(html);
        assert!(
            !out.contains(secret),
            "leaked `{secret}` in `{out}` for `{html}`"
        );
        assert!(
            out.contains(r#"value="""#),
            "expected redacted value=\"\" in `{out}` for `{html}`"
        );
    }
}

#[test]
fn sanitize_html_handles_html5_edge_cases_and_entities() {
    let cases = [
        (
            r#"<!----><input type="password" value="pw_empty_comment">"#,
            "pw_empty_comment",
        ),
        (
            r#"<!-- a --!--><input type="password" value="pw_double_dash_bang">"#,
            "pw_double_dash_bang",
        ),
        (
            r#"<!-- a --!b --><input type="password" value="pw_bang_not_end">"#,
            "pw_bang_not_end",
        ),
        (
            r#"a<b+c="unclosed <input type=password value=pw_punct>"#,
            "pw_punct",
        ),
        (
            r#"a<x <input type=password value=pw_unclosed_tag>"#,
            "pw_unclosed_tag",
        ),
        (
            r#"</div title="unclosed <input type=password value=pw_endtag_quote>"#,
            "pw_endtag_quote",
        ),
        (
            r#"<div ="unclosed <input type=password value=pw_leading_eq>"#,
            "pw_leading_eq",
        ),
        (
            r#"<div data-x="unclosed <input type=password value=pw_unclosed_eof>"#,
            "pw_unclosed_eof",
        ),
        (r#"<input type="password" value=/secret/pw>"#, "secret"),
        (
            r#"<input type="&#112;assword" value="pw_entity">"#,
            "pw_entity",
        ),
        (
            r#"<input type="&#x70;assword" value="pw_entity_hex">"#,
            "pw_entity_hex",
        ),
        (
            r#"<style>p { content: "unclosed < "; }</style><input type="password" value="pw_after_style">"#,
            "pw_after_style",
        ),
        (
            r#"<!-- <script> --><input type="password" value="pw_after_comment_script">"#,
            "pw_after_comment_script",
        ),
        (
            r#"<p>中文 < 测试</p><输入><p测试="值"><input title="密码=测试" type="password" value="秘密pw8">"#,
            "秘密pw8",
        ),
    ];
    for (html, secret) in cases {
        let out = sanitize_html(html);
        assert!(
            !out.contains(secret),
            "leaked `{secret}` in `{out}` for `{html}`"
        );
        assert!(
            out.contains(r#"value="""#),
            "expected redacted value=\"\" in `{out}` for `{html}`"
        );
    }
    let self_close_slash = sanitize_html(r#"<input type="password" value=/>"#);
    assert!(self_close_slash.contains(r#"value="""#));
    assert!(self_close_slash.contains("/>"));

    let attr_script = sanitize_html(
        r#"<div data-x="<script>alert(1)</script>"></div><input type="password" value="pw_after_attr_script">"#,
    );
    assert!(attr_script.contains(r#"<div data-x="<script>alert(1)</script>"></div>"#));
    assert!(!attr_script.contains("pw_after_attr_script"));
}

#[test]
fn sanitize_html_preserves_literal_attrs_and_comments() {
    let attr = r#"<div data-x="<input type=password value=pw>"></div>"#;
    assert_eq!(sanitize_html(attr), attr);

    let comment_then_real =
        r#"<!-- <input type="password" value="pw"> --><input type="password" value="real">"#;
    let out = sanitize_html(comment_then_real);
    assert!(out.starts_with(r#"<!-- <input type="password" value="pw"> -->"#));
    assert!(!out.contains("real"));
    assert!(out.ends_with(r#"<input type="password" value="">"#));

    let with_lt = r#"<input type="password" value="a<b_pw8" data-ok="1">"#;
    let out_lt = sanitize_html(with_lt);
    assert!(!out_lt.contains("a<b_pw8"));
    assert!(out_lt.contains(r#"data-ok="1""#));
}

#[test]
fn sanitize_html_closes_round7_html5_differential_bypasses() {
    let cases = [
        (
            r#"<input =="x='1" type="password" value="PWNED_EQ_SECRET" data-end="'">"#,
            "PWNED_EQ_SECRET",
            None,
        ),
        (
            r#"<style>body{}</style x="><div y='"><script>evilRaw()</script><input type="password" value="PWNED_RAWTEXT">"#,
            "PWNED_RAWTEXT",
            Some("evilRaw"),
        ),
        (
            r#"<style/><script>evilXhtml()</script><input type="password" value="PWNED_XHTML"><style>.ok{}</style>"#,
            "PWNED_XHTML",
            Some("evilXhtml"),
        ),
        (
            r#"<svg><foreignObject><script src="a.js"/>var SECRET="PWNED_FOREIGN_SCRIPT";</script><input type="password" value="PWNED_FOREIGN_PW"></foreignObject></svg>"#,
            "PWNED_FOREIGN_PW",
            Some("PWNED_FOREIGN_SCRIPT"),
        ),
        (
            r#"<svg><p><script src="a.js"/>var SECRET="PWNED_BREAKOUT";</script><input type="password" value="PWNED_BREAKOUT_PW"></svg>"#,
            "PWNED_BREAKOUT_PW",
            Some("PWNED_BREAKOUT"),
        ),
        (
            r#"<script><!-- <script> var x = "</script>"; var PASSKEY = "PWNED_DBL_ESC"; --></script><input type="password" value="PWNED_AFTER_DBL">"#,
            "PWNED_AFTER_DBL",
            Some("PWNED_DBL_ESC"),
        ),
        (
            r#"<svg><style><script>evilSvgStyle()</script><input type="password" value="PWNED_SVG_STYLE"></style></svg>"#,
            "PWNED_SVG_STYLE",
            Some("evilSvgStyle"),
        ),
    ];

    for (html, pw_secret, script_secret) in cases {
        let out = sanitize_html(html);
        assert!(
            !out.contains(pw_secret),
            "leaked password `{pw_secret}` in `{out}` for `{html}`"
        );
        if let Some(js_secret) = script_secret {
            assert!(
                !out.contains(js_secret),
                "leaked script `{js_secret}` in `{out}` for `{html}`"
            );
        }
    }
}

#[test]
fn sanitize_html_closes_round8_html5_differential_bypasses() {
    let cases: &[(&str, Option<&str>, Option<&str>)] = &[
        // [E-07] Unquoted `<` in tag name or start-tag attributes
        (
            r#"<script <a >stealEverything()</script>"#,
            None,
            Some("stealEverything"),
        ),
        (
            r#"<input value=PWNED_LT_UNQUOTED<x type="password">"#,
            Some("PWNED_LT_UNQUOTED"),
            None,
        ),
        (
            r#"<x <y='1 <div data-z="' ><script>evilLtQuote()</script><input type='password' value='PWNED_XY'><span a="1">"#,
            Some("PWNED_XY"),
            Some("evilLtQuote"),
        ),
        // [E-08] Custom element `.` or UTF-8 tag names
        (
            r#"<x.y data-a='<div data-b="'>hello</x.y><script>stealDot()</script><input type='password' value='PWNED_DOT'><span z="1">"#,
            Some("PWNED_DOT"),
            Some("stealDot"),
        ),
        // [E-09] Self-closing `<style/>` in HTML namespace & `<iframe>`/`<xmp>`/`<noembed>`/`<noframes>` RAWTEXT
        (
            r#"<style/>body{content:'<div x="'}</style><script>var K='LEAKED_STYLE_JS';</script><input type='password' value='LEAKED_STYLE_PW'><span title="x"></span>"#,
            Some("LEAKED_STYLE_PW"),
            Some("LEAKED_STYLE_JS"),
        ),
        (
            r#"<iframe><div title="</iframe><script>var K='PWNED_IFRAME_JS';</script><input type='password' value='PWNED_IFRAME_PW'><span title="x"></span>"#,
            Some("PWNED_IFRAME_PW"),
            Some("PWNED_IFRAME_JS"),
        ),
        (
            r#"<xmp><div title="</xmp><script>var K='PWNED_XMP_JS';</script><input type='password' value='PWNED_XMP_PW'><span title="x"></span>"#,
            Some("PWNED_XMP_PW"),
            Some("PWNED_XMP_JS"),
        ),
        // [E-10] Foreign `<![CDATA[`, `<annotation-xml>`, MathML `<mtext>`, `foreignObject` scope barrier, bogus `<?`, `<script><!-->`
        (
            r#"<svg><![CDATA[ > <p><div data-x=" ]]></svg><script>var K='PWNED_CDATA_JS';</script><input type='password' value='PWNED_CDATA_PW'><span data-y="x">"#,
            Some("PWNED_CDATA_PW"),
            Some("PWNED_CDATA_JS"),
        ),
        (
            r#"<math><annotation-xml><style><p><script>evilAnnot()</script><input type="password" value="PWNED_ANNOT_PW"></style></annotation-xml></math>"#,
            Some("PWNED_ANNOT_PW"),
            Some("evilAnnot"),
        ),
        (
            r#"<math><mtext><script src="a.js"/>var SECRET="PWNED_MTEXT_SCRIPT";</script></mtext></math>"#,
            None,
            Some("PWNED_MTEXT_SCRIPT"),
        ),
        (
            r#"<svg><foreignObject><div></svg></div></foreignObject><style><p><script>evilScope()</script><input type="password" value="PWNED_SCOPE_PW"></style></svg>"#,
            Some("PWNED_SCOPE_PW"),
            Some("evilScope"),
        ),
        (
            r#"<?xml <div data-x=" ?><script>stealPi()</script><input type='password' value='PWNED_PI'><span y="1">"#,
            Some("PWNED_PI"),
            Some("stealPi"),
        ),
        (
            r#"<script><!--><script></script><b x="--></script><div y='"><script>stealAbrupt2()</script><input type="password" value="PWNED_ABRUPT_2"><b z="'">"#,
            Some("PWNED_ABRUPT_2"),
            Some("stealAbrupt2"),
        ),
    ];

    for (html, pw_secret, script_secret) in cases {
        let out = sanitize_html(html);
        if let Some(pw) = pw_secret {
            assert!(!out.contains(pw), "leaked password `{pw}` in `{out}`");
        }
        if let Some(js) = script_secret {
            assert!(!out.contains(js), "leaked script `{js}` in `{out}`");
        }
    }
}

#[test]
fn sanitize_html_closes_round9_html5_and_xhtml_differentials() {
    let cases: &[(&str, &str, &str)] = &[
        (
            r#"<a+b data-x="<div data-y='">hello</a+b><script>var K="LEAK_R9_TAG_JS";</script><input type="password" value="LEAK_R9_TAG_PW"><span z="'">"#,
            "LEAK_R9_TAG_PW",
            "LEAK_R9_TAG_JS",
        ),
        (
            r#"<svg><font><style><p><script>var K="LEAK_R9_FONT_JS";</script><input type="password" value="LEAK_R9_FONT_PW"></style></font></svg>"#,
            "LEAK_R9_FONT_PW",
            "LEAK_R9_FONT_JS",
        ),
        (
            r#"<svg><font color="red"><style><div data-x="</style><script>var K="LEAK_R9_FONTCOL_JS";</script><input type="password" value="LEAK_R9_FONTCOL_PW"><span a="1">"#,
            "LEAK_R9_FONTCOL_PW",
            "LEAK_R9_FONTCOL_JS",
        ),
        (
            r#"<div><svg></div><style><div data-x="</style><script>var K="LEAK_R9_ANC_JS";</script><input type="password" value="LEAK_R9_ANC_PW"><span a="1">"#,
            "LEAK_R9_ANC_PW",
            "LEAK_R9_ANC_JS",
        ),
        (
            r#"<math><annotation-xml encoding=" text/html "><style><p><script>var K="LEAK_R9_AXTRIM_JS";</script><input type="password" value="LEAK_R9_AXTRIM_PW"></style></annotation-xml></math>"#,
            "LEAK_R9_AXTRIM_PW",
            "LEAK_R9_AXTRIM_JS",
        ),
        (
            r#"<math><annotation-xml encoding="text&sol;html"><style><div data-x="</style><script>var K="LEAK_R9_AXENT_JS";</script><input type="password" value="LEAK_R9_AXENT_PW"><span a="1"></annotation-xml></math>"#,
            "LEAK_R9_AXENT_PW",
            "LEAK_R9_AXENT_JS",
        ),
    ];

    for (html, pw, js) in cases {
        let out = sanitize_html(html);
        assert!(
            !out.contains(pw),
            "leaked password `{pw}` in `{out}` for `{html}`"
        );
        assert!(
            !out.contains(js),
            "leaked script `{js}` in `{out}` for `{html}`"
        );
    }

    for (xhtml, pw, js) in [
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><style/><!-- </style><div data-x=' --><script>var K="LEAK_R9_XHTML_JS";</script><input type="password" value="LEAK_R9_XHTML_PW"/><span title="'"/></body></html>"#,
            "LEAK_R9_XHTML_PW",
            "LEAK_R9_XHTML_JS",
        ),
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><script src="a.js"/><!-- </script><div data-x=' --><script>var K="LEAK_R9_XSCR_JS";</script><input type="password" value="LEAK_R9_XSCR_PW"/><span title="'"/></body></html>"#,
            "LEAK_R9_XSCR_PW",
            "LEAK_R9_XSCR_JS",
        ),
    ] {
        let out = super::sanitize_html_with_mode(xhtml, Some(true));
        assert!(!out.contains(pw), "leaked XHTML password `{pw}`: {out}");
        assert!(!out.contains(js), "leaked XHTML script `{js}`: {out}");
    }
}

#[test]
fn html5_xmlns_boilerplate_and_foreign_scope_select_differentials_never_leak() {
    let deep_span = format!(
        r#"{}<svg><style><!-- </style><div data-x=" --><script>var K='LEAK_512_JS';</script><foreignObject><input type='password' value='LEAK_512_PW'></foreignObject><g a='"'></svg>"#,
        "<span>".repeat(520)
    );
    let html_cases: Vec<(&str, &str, &str)> = vec![
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><![CDATA[ ><script>var K="LEAK_R10_CDATA_JS";</script><input type="password" value="LEAK_R10_CDATA_PW"> ]]></body></html>"#,
            "LEAK_R10_CDATA_PW",
            "LEAK_R10_CDATA_JS",
        ),
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><style/>body{content:"<div data-x='"}</style><script>var K="LEAK_R10_STYLE_JS";</script><input type="password" value="LEAK_R10_STYLE_PW"><span y="'"></body></html>"#,
            "LEAK_R10_STYLE_PW",
            "LEAK_R10_STYLE_JS",
        ),
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><svg><p><style><div data-x="</style><script>var K='LEAK_R10_SVGP_JS';</script><input type='password' value='LEAK_R10_SVGP_PW'><span a="1"></body></html>"#,
            "LEAK_R10_SVGP_PW",
            "LEAK_R10_SVGP_JS",
        ),
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><script src="a.js"/>var K="LEAK_R10_SCR_JS";</script></body></html>"#,
            "NOT_PRESENT_PW",
            "LEAK_R10_SCR_JS",
        ),
        (
            r#"<svg></p><style><div data-x="</style><script>var K='LEAK_ENDP_JS';</script><input type='password' value='LEAK_ENDP_PW'><span a="1">"#,
            "LEAK_ENDP_PW",
            "LEAK_ENDP_JS",
        ),
        (
            r#"<svg></br><style><div data-x="</style><script>var K='LEAK_ENDBR_JS';</script><input type='password' value='LEAK_ENDBR_PW'><span a="1">"#,
            "LEAK_ENDBR_PW",
            "LEAK_ENDBR_JS",
        ),
        (
            r#"<h1><svg></h2><style><div data-x="</style><script>var K='LEAK_H1H2_JS';</script><input type='password' value='LEAK_H1H2_PW'><span a="1">"#,
            "LEAK_H1H2_PW",
            "LEAK_H1H2_JS",
        ),
        (
            r#"<table><tr><td><svg></tr><style><div data-x="</style><script>var K='LEAK_TDTR_JS';</script><input type='password' value='LEAK_TDTR_PW'><span a="1"></table>"#,
            "LEAK_TDTR_PW",
            "LEAK_TDTR_JS",
        ),
        (
            r#"<select><svg><textarea><div data-x="</textarea><script>var K='LEAK_SEL_JS';</script><input type='password' value='LEAK_SEL_PW'><span a="1">"#,
            "LEAK_SEL_PW",
            "LEAK_SEL_JS",
        ),
        (
            r#"<svg><foreignObject><div><select></div></foreignObject></svg></select></div></foreignObject><style><!-- </style><div data-x=" --><script>var K='LEAK_FOSEL_JS';</script><foreignObject><input type='password' value='LEAK_FOSEL_PW'></foreignObject><g a='"'></svg>"#,
            "LEAK_FOSEL_PW",
            "LEAK_FOSEL_JS",
        ),
        (
            r#"<svg><script><![CDATA[ </script><g data-x=" ]]>var K='LEAK_SVGSCR_JS';</script><foreignObject><input type='password' value='LEAK_SVGSCR_PW'></foreignObject><g y='"'></svg>"#,
            "LEAK_SVGSCR_PW",
            "LEAK_SVGSCR_JS",
        ),
        (
            r#"<svg><script><!-- </script><g data-x=" -->var K='LEAK_SVGCMT_JS';</script><foreignObject><input type='password' value='LEAK_SVGCMT_PW'></foreignObject><g y='"'></svg>"#,
            "LEAK_SVGCMT_PW",
            "LEAK_SVGCMT_JS",
        ),
        (&deep_span, "LEAK_512_PW", "LEAK_512_JS"),
    ];
    for (html, pw, js) in html_cases {
        let out = sanitize_html(html);
        assert!(!out.contains(pw), "leaked password `{pw}` in `{out}`");
        assert!(!out.contains(js), "leaked script `{js}` in `{out}`");
    }
}

#[test]
fn xhtml_xml_processing_instructions_dtd_subsets_and_comments_never_leak() {
    let xhtml_cases = [
        (
            r#"<?pi ><div data-x=" ?><html xmlns="http://www.w3.org/1999/xhtml"><body><script>var K='LEAK_XPI_JS';</script><input type='password' value='LEAK_XPI_PW'/><span a='"'/></body></html>"#,
            "LEAK_XPI_PW",
            "LEAK_XPI_JS",
        ),
        (
            r#"<!DOCTYPE html [ <!-- ><div data-x=' --> ]><html xmlns="http://www.w3.org/1999/xhtml"><body><script>var K="LEAK_XDTD_JS";</script><input type="password" value="LEAK_XDTD_PW"/><span y="'"/></body></html>"#,
            "LEAK_XDTD_PW",
            "LEAK_XDTD_JS",
        ),
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><!--><div data-x=' --><script>var K="LEAK_XCMT_JS";</script><input type="password" value="LEAK_XCMT_PW"/><span y="'"/></body></html>"#,
            "LEAK_XCMT_PW",
            "LEAK_XCMT_JS",
        ),
    ];
    for (xhtml, pw, js) in xhtml_cases {
        let out = super::sanitize_html_with_mode(xhtml, Some(true));
        assert!(!out.contains(pw), "leaked XHTML password `{pw}` in `{out}`");
        assert!(!out.contains(js), "leaked XHTML script `{js}` in `{out}`");
    }
}

#[test]
fn round11_html5_select_and_foreign_script_noscript_differentials_never_leak() {
    let cases = [
        (
            r#"<select><style><option value="</style><div data-x="><script>var K='LEAK_SEL_STYLE_JS';</script><input type='password' value='LEAK_SEL_STYLE_PW'></option><span y="1"></select>"#,
            "LEAK_SEL_STYLE_PW",
            "LEAK_SEL_STYLE_JS",
        ),
        (
            r#"<select><iframe><option value="</iframe><div data-x="><script>var K='LEAK_SEL_IFR_JS';</script><input type='password' value='LEAK_SEL_IFR_PW'></option><span y="1"></select>"#,
            "LEAK_SEL_IFR_PW",
            "LEAK_SEL_IFR_JS",
        ),
        (
            r#"<svg><script></svg></script><style><div data-x="</style><script>var K='LEAK_SVGSCR_ANC_JS';</script><input type='password' value='LEAK_SVGSCR_ANC_PW'><span a="1">"#,
            "LEAK_SVGSCR_ANC_PW",
            "LEAK_SVGSCR_ANC_JS",
        ),
        (
            r#"<svg><noscript><g data-x="</noscript><div data-y="><foreignObject><script>var K='LEAK_SVGNOSCR_JS';</script><input type='password' value='LEAK_SVGNOSCR_PW'></foreignObject><g z="1"></noscript></svg>"#,
            "LEAK_SVGNOSCR_PW",
            "LEAK_SVGNOSCR_JS",
        ),
    ];
    for (html, pw, js) in cases {
        let out = sanitize_html(html);
        assert!(!out.contains(pw), "leaked `{pw}` in `{out}`");
        assert!(!out.contains(js), "leaked `{js}` in `{out}`");
    }
}

#[test]
fn round11_xhtml_dtd_pi_namespace_prefix_iframe_and_style_cdata_never_leak() {
    let xhtml_cases = [
        (
            r#"<!DOCTYPE html [ <?pi "?> ]><html xmlns="http://www.w3.org/1999/xhtml"><body><span title="a&gt;b"><script>var K='LEAK_XDTDPI_JS';</script><input type="password" value="LEAK_XDTDPI_PW"/></span><span y='"'/></body></html>"#,
            "LEAK_XDTDPI_PW",
            "LEAK_XDTDPI_JS",
        ),
        (
            r#"<x:html xmlns:x="http://www.w3.org/1999/xhtml"><x:body><x:script>var K='LEAK_XNS_JS';</x:script><x:input type="password" value="LEAK_XNS_PW"/></x:body></x:html>"#,
            "LEAK_XNS_PW",
            "LEAK_XNS_JS",
        ),
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><iframe><script>var K='LEAK_XIFR_JS';</script><input type="password" value="LEAK_XIFR_PW"/></iframe></body></html>"#,
            "LEAK_XIFR_PW",
            "LEAK_XIFR_JS",
        ),
        (
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><style><![CDATA[ a::after { content: "</style><div x='"; } ]]></style></head><body><script>var K="LEAK_XCDA_JS";</script><input type="password" value="LEAK_XCDA_PW"/><span y="'"/></body></html>"#,
            "LEAK_XCDA_PW",
            "LEAK_XCDA_JS",
        ),
    ];
    for (xhtml, pw, js) in xhtml_cases {
        let out = super::sanitize_html_with_mode(xhtml, Some(true));
        assert!(!out.contains(pw), "leaked XHTML `{pw}` in `{out}`");
        assert!(!out.contains(js), "leaked XHTML `{js}` in `{out}`");
    }
}

#[test]
fn round11_meta_and_xml_charset_prescan_prevent_multibyte_quote_consumption_bypass() {
    // ISO-2022-JP ESC $ B switches to 2-byte JIS X 0208 (0x21..=0x7E), consuming 0x22 ('"') into U+3001 '、'
    let mut raw = b"<meta charset=\"iso-2022-jp\"><div a=\"".to_vec();
    raw.extend_from_slice(b"\x1b$B!\"\x1b(B");
    raw.extend_from_slice(
        b" b=\"><script>var K='LEAK_ISO2022_JS';</script><input type='password' value='LEAK_ISO2022_PW'><span c=\"1\">",
    );
    let headers = vec![("content-type".to_string(), "text/html".to_string())];
    let (decoded, mode) = super::decode_nav_body_for_sanitize(&headers, &raw).unwrap();
    let out = super::sanitize_html_with_mode(&decoded, mode);
    assert!(
        !out.contains("LEAK_ISO2022_PW"),
        "leaked ISO-2022-JP password in `{out}`"
    );
    assert!(
        !out.contains("LEAK_ISO2022_JS"),
        "leaked ISO-2022-JP script in `{out}`"
    );

    let mut xml_raw =
        b"<?xml version=\"1.0\" encoding=\"iso-2022-jp\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><body><div a=\"".to_vec();
    xml_raw.extend_from_slice(b"\x1b$B!\"\x1b(B");
    xml_raw.extend_from_slice(
        b" b=\"1\"><script>var K='LEAK_XML_ISO_JS';</script><input type='password' value='LEAK_XML_ISO_PW'/></div><span c=\"1\"/></body></html>",
    );
    let xml_headers = vec![(
        "content-type".to_string(),
        "application/xhtml+xml".to_string(),
    )];
    let (xml_dec, xml_mode) = super::decode_nav_body_for_sanitize(&xml_headers, &xml_raw).unwrap();
    let xml_out = super::sanitize_html_with_mode(&xml_dec, xml_mode);
    assert!(!xml_out.contains("LEAK_XML_ISO_PW"), "{xml_out}");
    assert!(!xml_out.contains("LEAK_XML_ISO_JS"), "{xml_out}");
}

#[test]
fn round12_content_type_essence_and_meta_charset_prescan_prevent_differential_bypasses() {
    let mut multi_ct_raw = b"<div a=\"".to_vec();
    multi_ct_raw.extend_from_slice(b"\x1b$B!\"\x1b(B");
    multi_ct_raw.extend_from_slice(
        b" b=\"><script>var K='LEAK_R12_CT_JS';</script><input type='password' value='LEAK_R12_CT_PW'><span c=\"1\">",
    );
    let multi_headers = vec![
        ("Content-Type".to_string(), "text/plain".to_string()),
        (
            "content-type".to_string(),
            "text/html; foo=\"a; charset=utf-8\"; charset = \"iso-2022-jp\"".to_string(),
        ),
    ];
    let (dec1, mode1) = super::decode_nav_body_for_sanitize(&multi_headers, &multi_ct_raw).unwrap();
    let out1 = super::sanitize_html_with_mode(&dec1, mode1);
    assert!(!out1.contains("LEAK_R12_CT_PW"), "{out1}");
    assert!(!out1.contains("LEAK_R12_CT_JS"), "{out1}");

    // text/html ignores <?xml encoding=...?> bogus comment and decoy <meta> inside attribute values
    let mut html_with_bogus_xml =
        b"<?xml version=\"1.0\" encoding=\"iso-2022-jp\"?><html data-x=\"<meta charset='iso-2022-jp'>\">"
            .to_vec();
    html_with_bogus_xml.extend_from_slice(
        b"<body>\x1b$B!\"\x1b(B<script>var K='LEAK_R12_BOGUSXML_JS';</script><input type='password' value='LEAK_R12_BOGUSXML_PW'></body></html>",
    );
    let html_headers = vec![("content-type".to_string(), "text/html".to_string())];
    let (dec2, mode2) =
        super::decode_nav_body_for_sanitize(&html_headers, &html_with_bogus_xml).unwrap();
    let out2 = super::sanitize_html_with_mode(&dec2, mode2);
    assert!(!out2.contains("LEAK_R12_BOGUSXML_PW"), "{out2}");
    assert!(!out2.contains("LEAK_R12_BOGUSXML_JS"), "{out2}");

    // <meta http-equiv="content-type" content="charset = iso-2022-jp"> past 1024 bytes within 64 KiB
    let mut deep_meta = vec![b' '; 2048];
    deep_meta.extend_from_slice(
        b"<html data-x=\"<meta charset='utf-8'>\"><head><meta http-equiv='Content-Type' content='charset = iso-2022-jp'></head><div a=\"",
    );
    deep_meta.extend_from_slice(b"\x1b$B!\"\x1b(B");
    deep_meta.extend_from_slice(
        b" b=\"><script>var K='LEAK_R12_META64K_JS';</script><input type='password' value='LEAK_R12_META64K_PW'><span c=\"1\">",
    );
    let (dec3, mode3) = super::decode_nav_body_for_sanitize(&html_headers, &deep_meta).unwrap();
    let out3 = super::sanitize_html_with_mode(&dec3, mode3);
    assert!(!out3.contains("LEAK_R12_META64K_PW"), "{out3}");
    assert!(!out3.contains("LEAK_R12_META64K_JS"), "{out3}");
}

#[test]
fn round12_html5_foreign_stack_and_prefixed_xhtml_never_leak() {
    let cases = [
        (
            r#"<svg><g><foreignObject></g><style><!-- </style><div x=" --><script>var K='LEAK_R12_FG_JS';</script><foreignObject><input type='password' value='LEAK_R12_FG_PW'></foreignObject><g y='"'></svg>"#,
            "LEAK_R12_FG_PW",
            "LEAK_R12_FG_JS",
        ),
        (
            r#"<svg><foreignObject></svg></foreignObject><style>/*<div x="*/</style><script>var K='LEAK_R12_ENDSVG_JS';</script><input type='password' value='LEAK_R12_ENDSVG_PW'><span y="1">"#,
            "LEAK_R12_ENDSVG_PW",
            "LEAK_R12_ENDSVG_JS",
        ),
        (
            r#"<svg><g><foreignObject><p>hi<div></div></g><style><!-- </style><div x=" --><script>var K='LEAK_R12_IMPP_JS';</script><foreignObject><input type='password' value='LEAK_R12_IMPP_PW'></foreignObject><g y='"'></svg>"#,
            "LEAK_R12_IMPP_PW",
            "LEAK_R12_IMPP_JS",
        ),
        (
            r#"<x:html xmlns:x="http://www.w3.org/1999/xhtml"><x:body><x:script>var K='LEAK_R12_PREF_JS';</x:script><x:input type="PASSWORD" value="LEAK_R12_PREF_PW"/></x:body></x:html>"#,
            "LEAK_R12_PREF_PW",
            "LEAK_R12_PREF_JS",
        ),
    ];
    for (html, pw, js) in cases {
        let out = sanitize_html(html);
        assert!(!out.contains(pw), "leaked `{pw}` in `{out}`");
        assert!(!out.contains(js), "leaked `{js}` in `{out}`");
    }
}
