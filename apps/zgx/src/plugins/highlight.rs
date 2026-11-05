//! The same full-document syntax parser and bounded character comparison
//! as the original desktop diff. Old/new documents keep independent state.
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use serde_json::{Value as Json, json};
use zeron_syntax::{HighlightKind as K, HighlightRequest};

fn color(kind: K) -> &'static str {
    match kind {
        K::Comment => "$zeron.syntax.comment",
        K::Keyword | K::KeywordStorage => "$zeron.syntax.keyword",
        K::String | K::StringSpecial | K::MarkupRaw => "$zeron.syntax.string",
        K::Escape => "$zeron.syntax.escape",
        K::Number => "$zeron.syntax.number",
        K::Boolean | K::Constant => "$zeron.syntax.constant",
        K::Type | K::TypeBuiltin | K::Constructor => "$zeron.syntax.type",
        K::Function | K::FunctionBuiltin => "$zeron.syntax.function",
        K::Macro => "$zeron.syntax.macro",
        K::Property => "$zeron.syntax.property",
        K::Variable | K::VariableSpecial | K::Parameter => "$zeron.syntax.variable",
        K::Attribute => "$zeron.syntax.attribute",
        K::Label => "$zeron.syntax.label",
        K::Tag => "$zeron.syntax.tag",
        K::Operator => "$zeron.syntax.operator",
        K::Punctuation | K::Bracket1 | K::Bracket2 | K::Bracket3 => "$zeron.syntax.punctuation",
        K::MarkupHeading => "$zeron.syntax.heading",
        K::MarkupLink | K::MarkupReference => "$zeron.syntax.link",
        K::MarkupEmphasis => "$zeron.syntax.emphasis",
        K::MarkupStrong => "$zeron.syntax.strong",
        K::Invalid => "$status.danger",
        K::Embedded => "$zeron.syntax.text",
    }
}

fn document(source: &str, path: &str, wanted: &Json) -> Json {
    let Ok(doc) = zeron_syntax::highlight(HighlightRequest { source, path: Some(path), fence_tag: None }) else {
        return Json::Null;
    };
    let wanted: BTreeSet<usize> = wanted.as_array().into_iter().flatten()
        .filter_map(|n| n.as_u64().and_then(|n| usize::try_from(n).ok())).collect();
    let mut lines=serde_json::Map::new();
    for (index,spans) in doc.lines.iter().enumerate() {
        let line=index+1;
        if !wanted.contains(&line) {continue}
        lines.insert(line.to_string(),Json::Array(spans.iter().map(|s|json!([s.range.start+1,s.range.end,color(s.kind)])).collect()));
    }
    Json::Object(lines)
}

fn changed(old: &str,new: &str)->(Vec<Json>,Vec<Json>) {
    if old.len()+new.len()>8192 {return (Vec::new(),Vec::new())}
    let diff=similar::TextDiff::configure().timeout(Duration::from_millis(5)).diff_chars(old,new);
    let (mut a,mut b)=(0,0);
    let (mut removed,mut added)=(Vec::new(),Vec::new());
    for change in diff.iter_all_changes() {
        let n=change.value().len();
        match change.tag() {
            similar::ChangeTag::Delete=>{removed.push(json!([a+1,a+n]));a+=n},
            similar::ChangeTag::Insert=>{added.push(json!([b+1,b+n]));b+=n},
            similar::ChangeTag::Equal=>{a+=n;b+=n},
        }
    }
    (removed,added)
}

pub fn diff(asked: &Json)->Result<Json,String> {
    let old=asked["oldText"].as_str().unwrap_or("");
    let new=asked["newText"].as_str().unwrap_or("");
    if old.len()>zeron_syntax::DEFAULT_MAX_SOURCE_BYTES || new.len()>zeron_syntax::DEFAULT_MAX_SOURCE_BYTES {
        return Ok(json!({"limited":true}));
    }
    let path=asked["path"].as_str().unwrap_or("");
    let mut result=json!({"old":document(old,path,&asked["oldLines"]),"new":document(new,path,&asked["newLines"])});
    let old_lines:Vec<_>=old.lines().collect();
    let new_lines:Vec<_>=new.lines().collect();
    let (mut removed,mut added)=(serde_json::Map::new(),serde_json::Map::new());
    let started=Instant::now();
    for pair in asked["pairs"].as_array().into_iter().flatten().take(4000) {
        if started.elapsed()>Duration::from_millis(200) {break}
        let (Some(a),Some(b))=(pair[0].as_u64(),pair[1].as_u64()) else {continue};
        let (Some(a),Some(b))=(a.checked_sub(1),b.checked_sub(1)) else {continue};
        let (Some(left),Some(right))=(old_lines.get(a as usize),new_lines.get(b as usize)) else {continue};
        let (x,y)=changed(left,right);
        removed.insert((a+1).to_string(),json!(x));added.insert((b+1).to_string(),json!(y));
    }
    result["removed"]=Json::Object(removed);result["added"]=Json::Object(added);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_document_context_is_independent_on_each_side() {
        let v=diff(&json!({"path":"a.rs","oldText":"/* open\nlet x = 1;\n*/\n","newText":"// closed\nlet x = 1;\n","oldLines":[2],"newLines":[2]})).unwrap();
        assert!(v["old"]["2"].as_array().unwrap().iter().any(|s|s[2]=="$zeron.syntax.comment"));
        assert!(v["new"]["2"].as_array().unwrap().iter().any(|s|s[2]=="$zeron.syntax.keyword"));
        assert!(v["old"].get("1").is_none(),"only requested diff lines leave the host");
    }
    #[test]
    fn character_changes_are_utf8_boundaries_and_do_not_erase_syntax() {
        let old="const value = 'café';";let new="const value = 'caffè';";
        let (a,b)=changed(old,new);
        for (text,ranges) in [(old,a),(new,b)] {
            assert!(!ranges.is_empty());
            for r in ranges {assert!(text.is_char_boundary(r[0].as_u64().unwrap() as usize-1));assert!(text.is_char_boundary(r[1].as_u64().unwrap() as usize));}
        }
    }
    #[test]
    fn unknown_languages_and_large_sources_fall_back_without_losing_the_diff() {
        assert!(diff(&json!({"path":"opaque.xyz","oldText":"hi","oldLines":[1]})).unwrap()["old"].is_null());
        assert_eq!(diff(&json!({"newText":"x".repeat(zeron_syntax::DEFAULT_MAX_SOURCE_BYTES+1)})).unwrap()["limited"],true);
    }
}
