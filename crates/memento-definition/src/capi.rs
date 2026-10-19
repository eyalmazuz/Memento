//! C ABI entrypoints for `memento-definition`.
//!
//! Exposes:
//! - `memento_definition_split_css_tokens`: splits CSS value tokens.
//! - `memento_definition_free_string_list`: frees returned string list.
//! - `memento_definition_render_entries`: renders dictionary entries to rich text HTML.
//! - `memento_definition_free_string`: frees returned HTML string.

use std::collections::{HashMap, HashSet};
use std::ffi::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::Arc;

use crate::css::split_css_tokens;
use crate::element::{
    Context, CssAttributeOperator, CssAttributeSelector, CssCombinator, CssDeclaration,
    CssPseudoClass, CssPseudoClassSelector, CssPseudoElement, CssRule, CssSelectorPart,
    LayoutDirection, ParsedStylesheet, ScreenMetrics,
};
use crate::json::Json;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefSliceC {
    pub data: *const c_char,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefCssAttrSelectorC {
    pub op: i32,
    pub name: MementoDefSliceC,
    pub value: MementoDefSliceC,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefCssPseudoClassSelectorC {
    pub pseudo_type: i32,
    pub child_index: i32,
    pub negated: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefCssSelectorPartC {
    pub combinator: i32,
    pub tag: MementoDefSliceC,
    pub class_names: *const MementoDefSliceC,
    pub class_names_len: usize,
    pub attributes: *const MementoDefCssAttrSelectorC,
    pub attributes_len: usize,
    pub pseudo_classes: *const MementoDefCssPseudoClassSelectorC,
    pub pseudo_classes_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefCssDeclarationC {
    pub property: MementoDefSliceC,
    pub value: MementoDefSliceC,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefCssRuleC {
    pub selector: *const MementoDefCssSelectorPartC,
    pub selector_len: usize,
    pub declarations: *const MementoDefCssDeclarationC,
    pub declarations_len: usize,
    pub pseudo_element: i32,
    pub specificity: i32,
    pub order: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefTagRuleIndexesC {
    pub tag: MementoDefSliceC,
    pub indexes: *const usize,
    pub indexes_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefChildIndexRuleIndexesC {
    pub child_index: i64,
    pub indexes: *const usize,
    pub indexes_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefTagChildRuleIndexesC {
    pub tag: MementoDefSliceC,
    pub child_indexes: *const MementoDefChildIndexRuleIndexesC,
    pub child_indexes_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefParsedStylesheetC {
    pub rules: *const MementoDefCssRuleC,
    pub rules_len: usize,

    pub universal_rule_indexes: *const usize,
    pub universal_rule_indexes_len: usize,

    pub rule_indexes_by_target_tag: *const MementoDefTagRuleIndexesC,
    pub rule_indexes_by_target_tag_len: usize,

    pub non_nth_rule_indexes_by_target_tag: *const MementoDefTagRuleIndexesC,
    pub non_nth_rule_indexes_by_target_tag_len: usize,

    pub rule_indexes_by_target_tag_and_child_index: *const MementoDefTagChildRuleIndexesC,
    pub rule_indexes_by_target_tag_and_child_index_len: usize,

    pub uses_element_child_count: bool,

    pub element_child_count_tags: *const MementoDefSliceC,
    pub element_child_count_tags_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoDefDetailStateC {
    pub key_ptr: *const c_char,
    pub key_len: usize,
    pub open: bool,
}

pub type MementoDefImageSourceCb = unsafe extern "C" fn(
    path_ptr: *const c_char,
    path_len: usize,
    appearance_ptr: *const c_char,
    appearance_len: usize,
    text_color_ptr: *const c_char,
    text_color_len: usize,
    user_data: *mut std::ffi::c_void,
    out_ptr: *mut *mut c_char,
    out_len: *mut usize,
);

pub type MementoDefLocaleDirectionCb = unsafe extern "C" fn(
    lang_ptr: *const c_char,
    lang_len: usize,
    user_data: *mut std::ffi::c_void,
) -> i32;

#[repr(C)]
pub struct MementoDefRenderInputC {
    pub content_json_utf8: *const c_char,
    pub content_json_len: usize,

    pub style: i32,

    pub has_screen: bool,
    pub screen_dpi: f64,
    pub screen_width: f64,
    pub screen_height: f64,

    pub root_font_pixel_size: f64,

    pub glossary_text_color_ptr: *const c_char,
    pub glossary_text_color_len: usize,

    pub glossary_bg_color_ptr: *const c_char,
    pub glossary_bg_color_len: usize,

    pub basepath_ptr: *const c_char,
    pub basepath_len: usize,

    pub stylesheet: *const MementoDefParsedStylesheetC,

    pub detail_states: *const MementoDefDetailStateC,
    pub detail_states_len: usize,

    pub image_source_cb: Option<MementoDefImageSourceCb>,
    pub locale_direction_cb: Option<MementoDefLocaleDirectionCb>,
    pub user_data: *mut std::ffi::c_void,
}

#[repr(C)]
pub struct MementoDefStringC {
    pub data: *mut c_char,
    pub len: usize,
    pub capacity: usize,
}

#[repr(C)]
pub struct MementoDefStringListC {
    pub items: *mut MementoDefStringC,
    pub len: usize,
    pub capacity: usize,
}

unsafe fn slice_to_str<'a>(slice: &'a MementoDefSliceC) -> Result<&'a str, ()> {
    if slice.data.is_null() || slice.len == 0 {
        Ok("")
    } else {
        let bytes = std::slice::from_raw_parts(slice.data as *const u8, slice.len);
        std::str::from_utf8(bytes).map_err(|_| ())
    }
}

unsafe fn convert_stylesheet(raw: &MementoDefParsedStylesheetC) -> Result<ParsedStylesheet, ()> {
    let mut rules = Vec::with_capacity(raw.rules_len);
    if !raw.rules.is_null() && raw.rules_len > 0 {
        for i in 0..raw.rules_len {
            let r = &*raw.rules.add(i);
            let mut selector = Vec::with_capacity(r.selector_len);
            if !r.selector.is_null() && r.selector_len > 0 {
                for j in 0..r.selector_len {
                    let part = &*r.selector.add(j);
                    let tag = slice_to_str(&part.tag)?.to_string();
                    let mut class_names = HashSet::with_capacity(part.class_names_len);
                    if !part.class_names.is_null() && part.class_names_len > 0 {
                        for k in 0..part.class_names_len {
                            let cn = &*part.class_names.add(k);
                            let s = slice_to_str(cn)?;
                            if !s.is_empty() {
                                class_names.insert(s.to_string());
                            }
                        }
                    }
                    let mut attributes = Vec::with_capacity(part.attributes_len);
                    if !part.attributes.is_null() && part.attributes_len > 0 {
                        for k in 0..part.attributes_len {
                            let a = &*part.attributes.add(k);
                            let name = slice_to_str(&a.name)?.to_string();
                            let value = slice_to_str(&a.value)?.to_string();
                            let op = match a.op {
                                1 => CssAttributeOperator::Equals,
                                2 => CssAttributeOperator::StartsWith,
                                _ => CssAttributeOperator::Exists,
                            };
                            attributes.push(CssAttributeSelector { name, value, op });
                        }
                    }
                    let mut pseudo_classes = Vec::with_capacity(part.pseudo_classes_len);
                    if !part.pseudo_classes.is_null() && part.pseudo_classes_len > 0 {
                        for k in 0..part.pseudo_classes_len {
                            let pc = &*part.pseudo_classes.add(k);
                            let pseudo_type = match pc.pseudo_type {
                                1 => CssPseudoClass::LastChild,
                                2 => CssPseudoClass::NthChild,
                                _ => CssPseudoClass::FirstChild,
                            };
                            pseudo_classes.push(CssPseudoClassSelector {
                                pseudo_type,
                                child_index: pc.child_index,
                                negated: pc.negated,
                            });
                        }
                    }
                    let combinator = match part.combinator {
                        1 => CssCombinator::Child,
                        2 => CssCombinator::AdjacentSibling,
                        3 => CssCombinator::GeneralSibling,
                        _ => CssCombinator::Descendant,
                    };
                    selector.push(CssSelectorPart {
                        combinator,
                        tag,
                        class_names,
                        attributes,
                        pseudo_classes,
                    });
                }
            }
            let mut declarations = Vec::with_capacity(r.declarations_len);
            if !r.declarations.is_null() && r.declarations_len > 0 {
                for j in 0..r.declarations_len {
                    let decl = &*r.declarations.add(j);
                    let property = slice_to_str(&decl.property)?.to_string();
                    let value = slice_to_str(&decl.value)?.to_string();
                    declarations.push(CssDeclaration { property, value });
                }
            }
            let pseudo_element = match r.pseudo_element {
                1 => CssPseudoElement::Before,
                2 => CssPseudoElement::After,
                _ => CssPseudoElement::None,
            };
            rules.push(CssRule {
                selector,
                declarations,
                pseudo_element,
                specificity: r.specificity,
                order: r.order,
            });
        }
    }

    let universal_rule_indexes = if !raw.universal_rule_indexes.is_null() && raw.universal_rule_indexes_len > 0 {
        std::slice::from_raw_parts(raw.universal_rule_indexes, raw.universal_rule_indexes_len).to_vec()
    } else {
        Vec::new()
    };

    let mut rule_indexes_by_target_tag = HashMap::with_capacity(raw.rule_indexes_by_target_tag_len);
    if !raw.rule_indexes_by_target_tag.is_null() && raw.rule_indexes_by_target_tag_len > 0 {
        for i in 0..raw.rule_indexes_by_target_tag_len {
            let entry = &*raw.rule_indexes_by_target_tag.add(i);
            let tag = slice_to_str(&entry.tag)?.to_string();
            let idxs = if !entry.indexes.is_null() && entry.indexes_len > 0 {
                std::slice::from_raw_parts(entry.indexes, entry.indexes_len).to_vec()
            } else {
                Vec::new()
            };
            rule_indexes_by_target_tag.insert(tag, idxs);
        }
    }

    let mut non_nth_rule_indexes_by_target_tag = HashMap::with_capacity(raw.non_nth_rule_indexes_by_target_tag_len);
    if !raw.non_nth_rule_indexes_by_target_tag.is_null() && raw.non_nth_rule_indexes_by_target_tag_len > 0 {
        for i in 0..raw.non_nth_rule_indexes_by_target_tag_len {
            let entry = &*raw.non_nth_rule_indexes_by_target_tag.add(i);
            let tag = slice_to_str(&entry.tag)?.to_string();
            let idxs = if !entry.indexes.is_null() && entry.indexes_len > 0 {
                std::slice::from_raw_parts(entry.indexes, entry.indexes_len).to_vec()
            } else {
                Vec::new()
            };
            non_nth_rule_indexes_by_target_tag.insert(tag, idxs);
        }
    }

    let mut rule_indexes_by_target_tag_and_child_index = HashMap::with_capacity(raw.rule_indexes_by_target_tag_and_child_index_len);
    if !raw.rule_indexes_by_target_tag_and_child_index.is_null() && raw.rule_indexes_by_target_tag_and_child_index_len > 0 {
        for i in 0..raw.rule_indexes_by_target_tag_and_child_index_len {
            let entry = &*raw.rule_indexes_by_target_tag_and_child_index.add(i);
            let tag = slice_to_str(&entry.tag)?.to_string();
            let mut inner_map = HashMap::with_capacity(entry.child_indexes_len);
            if !entry.child_indexes.is_null() && entry.child_indexes_len > 0 {
                for j in 0..entry.child_indexes_len {
                    let c_entry = &*entry.child_indexes.add(j);
                    let idxs = if !c_entry.indexes.is_null() && c_entry.indexes_len > 0 {
                        std::slice::from_raw_parts(c_entry.indexes, c_entry.indexes_len).to_vec()
                    } else {
                        Vec::new()
                    };
                    inner_map.insert(c_entry.child_index, idxs);
                }
            }
            rule_indexes_by_target_tag_and_child_index.insert(tag, inner_map);
        }
    }

    let mut element_child_count_tags = HashSet::with_capacity(raw.element_child_count_tags_len);
    if !raw.element_child_count_tags.is_null() && raw.element_child_count_tags_len > 0 {
        for i in 0..raw.element_child_count_tags_len {
            let t = &*raw.element_child_count_tags.add(i);
            let tag = slice_to_str(t)?.to_string();
            element_child_count_tags.insert(tag);
        }
    }

    Ok(ParsedStylesheet {
        rules,
        universal_rule_indexes,
        rule_indexes_by_target_tag,
        non_nth_rule_indexes_by_target_tag,
        rule_indexes_by_target_tag_and_child_index,
        uses_element_child_count: raw.uses_element_child_count,
        element_child_count_tags,
        diagnostics: Vec::new(),
    })
}

/// Split CSS value string into whitespace-delimited top-level tokens.
#[no_mangle]
pub unsafe extern "C" fn memento_definition_split_css_tokens(
    utf8_ptr: *const c_char,
    utf8_len: usize,
    out: *mut MementoDefStringListC,
) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe {
        (*out).items = ptr::null_mut();
        (*out).len = 0;
        (*out).capacity = 0;
    }
    let Ok(result) = catch_unwind(AssertUnwindSafe(|| {
        let text = if utf8_len == 0 {
            ""
        } else {
            if utf8_ptr.is_null() {
                return None;
            }
            let slice = unsafe { std::slice::from_raw_parts(utf8_ptr as *const u8, utf8_len) };
            std::str::from_utf8(slice).ok()?
        };
        let tokens = split_css_tokens(text);
        let mut list_vec: Vec<MementoDefStringC> = Vec::with_capacity(tokens.len());
        for mut t in tokens {
            let item = MementoDefStringC {
                data: t.as_mut_ptr() as *mut c_char,
                len: t.len(),
                capacity: t.capacity(),
            };
            std::mem::forget(t);
            list_vec.push(item);
        }
        let (items_ptr, len, capacity) = (list_vec.as_mut_ptr(), list_vec.len(), list_vec.capacity());
        std::mem::forget(list_vec);
        Some(MementoDefStringListC {
            items: items_ptr,
            len,
            capacity,
        })
    })) else {
        return false;
    };
    if let Some(list) = result {
        unsafe {
            *out = list;
        }
        true
    } else {
        false
    }
}

/// Free string list allocated by `memento_definition_split_css_tokens`.
#[no_mangle]
pub unsafe extern "C" fn memento_definition_free_string_list(
    out: *mut MementoDefStringListC,
) {
    if out.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let list = unsafe { &mut *out };
        if !list.items.is_null() {
            let items = unsafe {
                Vec::from_raw_parts(list.items, list.len, list.capacity)
            };
            for item in items {
                if !item.data.is_null() {
                    drop(unsafe {
                        Vec::from_raw_parts(item.data as *mut u8, item.len, item.capacity)
                    });
                }
            }
            list.items = ptr::null_mut();
            list.len = 0;
            list.capacity = 0;
        }
    }));
}

/// Render structured and legacy entries to rich-text HTML string.
#[no_mangle]
pub unsafe extern "C" fn memento_definition_render_entries(
    input: *const MementoDefRenderInputC,
    out_html: *mut MementoDefStringC,
) -> bool {
    if input.is_null() || out_html.is_null() {
        return false;
    }
    unsafe {
        (*out_html).data = ptr::null_mut();
        (*out_html).len = 0;
        (*out_html).capacity = 0;
    }
    let Ok(result) = catch_unwind(AssertUnwindSafe(|| {
        let inp = unsafe { &*input };
        let json_text = if inp.content_json_len == 0 {
            "[]"
        } else {
            if inp.content_json_utf8.is_null() {
                return None;
            }
            let slice = unsafe {
                std::slice::from_raw_parts(inp.content_json_utf8 as *const u8, inp.content_json_len)
            };
            std::str::from_utf8(slice).ok()?
        };
        let parsed_json = Json::parse(json_text).ok()?;
        let entries: &[Json] = match &parsed_json {
            Json::Array(arr) => arr.as_slice(),
            _ => return None,
        };

        let style = match inp.style {
            0 => crate::content::LegacyGlossaryStyle::Bullet,
            1 => crate::content::LegacyGlossaryStyle::LineBreak,
            2 => crate::content::LegacyGlossaryStyle::Pipe,
            _ => crate::content::LegacyGlossaryStyle::NoSeparator,
        };

        let mut ctx = Context::default();
        if inp.has_screen {
            ctx.screen = Some(ScreenMetrics {
                available_width: inp.screen_width as i32,
                available_height: inp.screen_height as i32,
                logical_dpi: inp.screen_dpi,
            });
        } else {
            ctx.screen = None;
        }
        ctx.root_font_pixel_size = inp.root_font_pixel_size;
        ctx.parent_font_pixel_size = inp.root_font_pixel_size;

        if !inp.glossary_text_color_ptr.is_null() && inp.glossary_text_color_len > 0 {
            let slice = unsafe {
                std::slice::from_raw_parts(inp.glossary_text_color_ptr as *const u8, inp.glossary_text_color_len)
            };
            if let Ok(s) = std::str::from_utf8(slice) {
                ctx.glossary_text_color = s.to_string();
                ctx.text_color = s.to_string();
            }
        }
        if !inp.glossary_bg_color_ptr.is_null() && inp.glossary_bg_color_len > 0 {
            let slice = unsafe {
                std::slice::from_raw_parts(inp.glossary_bg_color_ptr as *const u8, inp.glossary_bg_color_len)
            };
            if let Ok(s) = std::str::from_utf8(slice) {
                ctx.glossary_background_color = s.to_string();
                ctx.background_color = s.to_string();
                ctx.painted_background_color = s.to_string();
            }
        }

        if !inp.stylesheet.is_null() {
            let sheet = unsafe { convert_stylesheet(&*inp.stylesheet).ok()? };
            ctx.needs_element_child_count = sheet.uses_element_child_count;
            ctx.dictionary_styles = Some(Arc::new(sheet));
        }

        if !inp.detail_states.is_null() && inp.detail_states_len > 0 {
            for i in 0..inp.detail_states_len {
                let ds = unsafe { &*inp.detail_states.add(i) };
                if !ds.key_ptr.is_null() && ds.key_len > 0 {
                    let slice = unsafe {
                        std::slice::from_raw_parts(ds.key_ptr as *const u8, ds.key_len)
                    };
                    if let Ok(key) = std::str::from_utf8(slice) {
                        ctx.detail_states.insert(key.to_string(), ds.open);
                    }
                }
            }
        }

        let basepath = if !inp.basepath_ptr.is_null() && inp.basepath_len > 0 {
            let slice = unsafe {
                std::slice::from_raw_parts(inp.basepath_ptr as *const u8, inp.basepath_len)
            };
            std::str::from_utf8(slice).unwrap_or("").to_string()
        } else {
            String::new()
        };

        let image_source_cb = inp.image_source_cb;
        let locale_direction_cb = inp.locale_direction_cb;
        let user_data = inp.user_data;

        let mut image_cb = |obj: &Json, ctx: &Context| -> String {
            let path = obj.get("path").as_str();
            let appearance = obj.get("appearance").as_str();
            let text_color = &ctx.text_color;
            if let Some(cb) = image_source_cb {
                let mut out_ptr: *mut c_char = ptr::null_mut();
                let mut out_len: usize = 0;
                unsafe {
                    cb(
                        path.as_ptr() as *const c_char,
                        path.len(),
                        appearance.as_ptr() as *const c_char,
                        appearance.len(),
                        text_color.as_ptr() as *const c_char,
                        text_color.len(),
                        user_data,
                        &mut out_ptr,
                        &mut out_len,
                    );
                    if !out_ptr.is_null() && out_len > 0 {
                        let slice = std::slice::from_raw_parts(out_ptr as *const u8, out_len);
                        String::from_utf8_lossy(slice).into_owned()
                    } else {
                        String::new()
                    }
                }
            } else {
                format!("{}{}", basepath, path)
            }
        };

        let mut locale_cb = |lang: &str| -> LayoutDirection {
            if let Some(cb) = locale_direction_cb {
                let code = unsafe {
                    cb(lang.as_ptr() as *const c_char, lang.len(), user_data)
                };
                if code == 1 {
                    LayoutDirection::RightToLeft
                } else {
                    LayoutDirection::LeftToRight
                }
            } else {
                LayoutDirection::LeftToRight
            }
        };

        let renderer = crate::css::Renderer::new();
        let mut html = renderer.render_entries(
            entries,
            style,
            &mut ctx,
            &mut image_cb,
            &mut locale_cb,
        );

        let res = MementoDefStringC {
            data: html.as_mut_ptr() as *mut c_char,
            len: html.len(),
            capacity: html.capacity(),
        };
        std::mem::forget(html);
        Some(res)
    })) else {
        return false;
    };

    if let Some(res) = result {
        unsafe {
            *out_html = res;
        }
        true
    } else {
        false
    }
}

/// Free HTML string allocated by `memento_definition_render_entries`.
#[no_mangle]
pub unsafe extern "C" fn memento_definition_free_string(
    out: *mut MementoDefStringC,
) {
    if out.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let str_struct = unsafe { &mut *out };
        if !str_struct.data.is_null() {
            drop(unsafe {
                Vec::from_raw_parts(str_struct.data as *mut u8, str_struct.len, str_struct.capacity)
            });
            str_struct.data = ptr::null_mut();
            str_struct.len = 0;
            str_struct.capacity = 0;
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_capi_split_tokens() {
        let text = CString::new("1px solid red calc(1 + 2)").unwrap();
        let mut list = MementoDefStringListC {
            items: ptr::null_mut(),
            len: 0,
            capacity: 0,
        };
        unsafe {
            let ok = memento_definition_split_css_tokens(
                text.as_ptr(),
                text.as_bytes().len(),
                &mut list,
            );
            assert!(ok);
            assert_eq!(list.len, 4);
            let s0 = std::slice::from_raw_parts((*list.items.add(0)).data as *const u8, (*list.items.add(0)).len);
            let s1 = std::slice::from_raw_parts((*list.items.add(1)).data as *const u8, (*list.items.add(1)).len);
            let s2 = std::slice::from_raw_parts((*list.items.add(2)).data as *const u8, (*list.items.add(2)).len);
            let s3 = std::slice::from_raw_parts((*list.items.add(3)).data as *const u8, (*list.items.add(3)).len);
            assert_eq!(std::str::from_utf8(s0).unwrap(), "1px");
            assert_eq!(std::str::from_utf8(s1).unwrap(), "solid");
            assert_eq!(std::str::from_utf8(s2).unwrap(), "red");
            assert_eq!(std::str::from_utf8(s3).unwrap(), "calc(1 + 2)");

            memento_definition_free_string_list(&mut list);
            assert!(list.items.is_null());
        }
    }

    #[test]
    fn test_capi_render_entries_simple() {
        let json = CString::new(r#"["hello", {"type":"text","text":"world"}]"#).unwrap();
        let color = CString::new("#000000").unwrap();
        let bg = CString::new("#ffffff").unwrap();
        let basepath = CString::new("file:///dict/").unwrap();

        let mut input = MementoDefRenderInputC {
            content_json_utf8: json.as_ptr(),
            content_json_len: json.as_bytes().len(),
            style: 0,
            has_screen: false,
            screen_dpi: 96.0,
            screen_width: 1920.0,
            screen_height: 1080.0,
            root_font_pixel_size: 14.0,
            glossary_text_color_ptr: color.as_ptr(),
            glossary_text_color_len: color.as_bytes().len(),
            glossary_bg_color_ptr: bg.as_ptr(),
            glossary_bg_color_len: bg.as_bytes().len(),
            basepath_ptr: basepath.as_ptr(),
            basepath_len: basepath.as_bytes().len(),
            stylesheet: ptr::null(),
            detail_states: ptr::null(),
            detail_states_len: 0,
            image_source_cb: None,
            locale_direction_cb: None,
            user_data: ptr::null_mut(),
        };

        let mut out = MementoDefStringC {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        };

        unsafe {
            let ok = memento_definition_render_entries(&input, &mut out);
            assert!(ok);
            assert!(!out.data.is_null());
            let bytes = std::slice::from_raw_parts(out.data as *const u8, out.len);
            let s = std::str::from_utf8(bytes).unwrap().to_owned();
            assert!(s.starts_with("<html><head></head><body>"));
            assert!(s.ends_with("</body></html>"));
            assert!(s.contains("hello"));
            assert!(s.contains("world"));
            memento_definition_free_string(&mut out);
            assert!(out.data.is_null());
            input.style = 99;
            let ok = memento_definition_render_entries(&input, &mut out);
            assert!(ok);
            let bytes = std::slice::from_raw_parts(out.data as *const u8, out.len);
            let s = std::str::from_utf8(bytes).unwrap();
            assert_eq!(s, "<html><head></head><body>helloworld</body></html>");
            memento_definition_free_string(&mut out);
            assert!(out.data.is_null());
        }
    }
}
