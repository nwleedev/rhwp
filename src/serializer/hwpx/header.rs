//! Contents/header.xml — DocInfo 리소스 테이블 동적 직렬화.
//!
//! Stage 1 (#182): IR의 `doc_info` 에 담긴 리소스를 역방향으로 HWPX XML로 출력한다.
//! IR이 비어있으면 해당 섹션도 비어있게 출력한다 (IR에 없는 리소스를 자동 생성하지 않음).
//!
//! 속성·자식 순서는 한컴 OWPML 공식 구현(hancom-io/hwpx-owpml-model, Apache 2.0)의
//! `Class/Head/*.cpp` 파일 `WriteElement()`, `InitMap()` 을 기준으로 맞춘다.
//!
//! ## 범위
//!
//! - 1단계 목표: 기존 HWPX 문서를 parse→serialize 했을 때 한컴2020이 온전히 다시 연다
//! - 완전히 새 빈 문서 생성은 1단계 범위 밖 (기본값 채우기 로직 없음)

use std::io::Write;

use quick_xml::Writer;

use crate::model::document::{DocInfo, DocProperties, Document};
use crate::model::style::{
    Alignment, BorderFill, BorderLine, BorderLineType, CharShape, DiagonalLine, FillType, Font,
    GradientFill, HeadType, LineSpacingType, Numbering, NumberingHead, ParaShape, SolidFill, Style,
    TabDef,
};
use crate::model::ColorRef;

use super::canonical_defaults::FONTFACE_LANG_NAMES;
use super::context::SerializeContext;
use super::utils::{empty_tag, end_tag, start_tag_attrs, write_xml_decl};
use super::SerializeError;

/// `header.xml` 바이트 생성. Stage 1 진입점.
pub fn write_header(doc: &Document, ctx: &SerializeContext) -> Result<Vec<u8>, SerializeError> {
    let mut w: Writer<Vec<u8>> = Writer::new(Vec::new());
    write_xml_decl(&mut w)?;

    // <hh:head> 루트 + 전체 네임스페이스 (parser가 기대하는 접두어 모두 선언)
    let sec_cnt = doc.doc_properties.section_count.max(1).to_string();
    start_tag_attrs(
        &mut w,
        "hh:head",
        &[
            ("xmlns:ha", "http://www.hancom.co.kr/hwpml/2011/app"),
            ("xmlns:hp", "http://www.hancom.co.kr/hwpml/2011/paragraph"),
            ("xmlns:hp10", "http://www.hancom.co.kr/hwpml/2016/paragraph"),
            ("xmlns:hs", "http://www.hancom.co.kr/hwpml/2011/section"),
            ("xmlns:hc", "http://www.hancom.co.kr/hwpml/2011/core"),
            ("xmlns:hh", "http://www.hancom.co.kr/hwpml/2011/head"),
            ("xmlns:hhs", "http://www.hancom.co.kr/hwpml/2011/history"),
            ("xmlns:hm", "http://www.hancom.co.kr/hwpml/2011/master-page"),
            ("xmlns:dc", "http://purl.org/dc/elements/1.1/"),
            ("xmlns:opf", "http://www.idpf.org/2007/opf/"),
            ("xmlns:epub", "http://www.idpf.org/2007/ops"),
            (
                "xmlns:ooxmlchart",
                "http://www.hancom.co.kr/hwpml/2016/ooxmlchart",
            ),
            ("xmlns:hpf", "http://www.hancom.co.kr/schema/2011/hpf"),
            (
                "xmlns:config",
                "urn:oasis:names:tc:opendocument:xmlns:config:1.0",
            ),
            ("version", "1.2"),
            ("secCnt", &sec_cnt),
        ],
    )?;

    write_begin_num(&mut w, &doc.doc_properties)?;

    // <hh:refList>: 모든 리소스 테이블을 감싸는 컨테이너
    super::utils::start_tag(&mut w, "hh:refList")?;
    write_fontfaces(&mut w, &doc.doc_info)?;
    write_border_fills(&mut w, &doc.doc_info, ctx)?;
    write_char_properties(&mut w, &doc.doc_info, ctx)?;
    write_tab_properties(&mut w, &doc.doc_info)?;
    write_numberings(&mut w, &doc.doc_info)?;
    write_para_properties(&mut w, &doc.doc_info, ctx)?;
    write_styles(&mut w, &doc.doc_info, ctx)?;
    end_tag(&mut w, "hh:refList")?;

    write_compatible_document(&mut w)?;
    write_doc_option(&mut w)?;
    write_track_change_config(&mut w)?;

    end_tag(&mut w, "hh:head")?;
    Ok(w.into_inner())
}

// =====================================================================
// <hh:beginNum>
// =====================================================================
fn write_begin_num<W: Write>(
    w: &mut Writer<W>,
    props: &DocProperties,
) -> Result<(), SerializeError> {
    empty_tag(
        w,
        "hh:beginNum",
        &[
            ("page", &props.page_start_num.max(1).to_string()),
            ("footnote", &props.footnote_start_num.max(1).to_string()),
            ("endnote", &props.endnote_start_num.max(1).to_string()),
            ("pic", &props.picture_start_num.max(1).to_string()),
            ("tbl", &props.table_start_num.max(1).to_string()),
            ("equation", &props.equation_start_num.max(1).to_string()),
        ],
    )
}

// =====================================================================
// <hh:fontfaces> — 7 언어 그룹
// =====================================================================
fn write_fontfaces<W: Write>(w: &mut Writer<W>, doc_info: &DocInfo) -> Result<(), SerializeError> {
    // IR의 font_faces는 항상 7개 언어 그룹을 유지한다고 기대하나,
    // 비어있거나 크기가 다를 수 있으므로 안전하게 처리.
    let groups: Vec<&Vec<Font>> = (0..7)
        .map(|i| doc_info.font_faces.get(i).unwrap_or(&EMPTY_FONT_VEC))
        .collect();

    let item_cnt = groups.iter().filter(|g| !g.is_empty()).count();
    if item_cnt == 0 {
        return Ok(());
    }

    start_tag_attrs(
        w,
        "hh:fontfaces",
        &[(
            "itemCnt",
            &groups.iter().filter(|g| !g.is_empty()).count().to_string(),
        )],
    )?;
    for (lang_idx, fonts) in groups.iter().enumerate() {
        if fonts.is_empty() {
            continue;
        }
        let lang = FONTFACE_LANG_NAMES[lang_idx];
        start_tag_attrs(
            w,
            "hh:fontface",
            &[("lang", lang), ("fontCnt", &fonts.len().to_string())],
        )?;
        for (id, font) in fonts.iter().enumerate() {
            let id_s = id.to_string();
            let attrs = [
                ("id", id_s.as_str()),
                ("face", font.name.as_str()),
                ("type", font_type_str(font.alt_type)),
                ("isEmbedded", "0"),
            ];
            if let Some(type_info) = font.type_info {
                start_tag_attrs(w, "hh:font", &attrs)?;
                write_font_type_info(w, type_info)?;
                end_tag(w, "hh:font")?;
            } else {
                empty_tag(w, "hh:font", &attrs)?;
            }
        }
        end_tag(w, "hh:fontface")?;
    }
    end_tag(w, "hh:fontfaces")?;
    Ok(())
}

static EMPTY_FONT_VEC: Vec<Font> = Vec::new();

fn write_font_type_info<W: Write>(
    w: &mut Writer<W>,
    type_info: [u8; 10],
) -> Result<(), SerializeError> {
    empty_tag(
        w,
        "hh:typeInfo",
        &[
            ("familyType", font_family_type_str(type_info[0])),
            ("weight", &type_info[2].to_string()),
            ("proportion", &type_info[3].to_string()),
            ("contrast", &type_info[4].to_string()),
            ("strokeVariation", &type_info[5].to_string()),
            ("armStyle", &type_info[6].to_string()),
            ("letterform", &type_info[7].to_string()),
            ("midline", &type_info[8].to_string()),
            ("xHeight", &type_info[9].to_string()),
        ],
    )
}

fn font_family_type_str(value: u8) -> &'static str {
    match value {
        1 => "FCAT_MYUNGJO",
        2 => "FCAT_GOTHIC",
        3 => "FCAT_SSERIF",
        4 => "FCAT_BRUSHSCRIPT",
        5 => "FCAT_DECORATIVE",
        6 => "FCAT_NONRECTMJ",
        7 => "FCAT_NONRECTGT",
        _ => "FCAT_UNKNOWN",
    }
}

fn font_type_str(alt_type: u8) -> &'static str {
    match alt_type {
        1 => "TTF",
        2 => "HFT",
        _ => "TTF", // 기본: TTF (한컴 샘플 관찰값)
    }
}

// =====================================================================
// <hh:borderFills>
// =====================================================================
fn write_border_fills<W: Write>(
    w: &mut Writer<W>,
    doc_info: &DocInfo,
    ctx: &SerializeContext,
) -> Result<(), SerializeError> {
    let _ = ctx;
    if doc_info.border_fills.is_empty() {
        return Ok(());
    }
    start_tag_attrs(
        w,
        "hh:borderFills",
        &[("itemCnt", &doc_info.border_fills.len().to_string())],
    )?;
    // HWPX borderFill의 id는 1부터 시작 (관찰값: ref_empty.hwpx).
    // 그러나 rhwp parser는 인덱스 기반으로 저장하므로 id는 배열 인덱스 그대로 사용.
    for (idx, bf) in doc_info.border_fills.iter().enumerate() {
        write_border_fill(w, idx as u16, bf)?;
    }
    end_tag(w, "hh:borderFills")?;
    Ok(())
}

fn write_border_fill<W: Write>(
    w: &mut Writer<W>,
    id: u16,
    bf: &BorderFill,
) -> Result<(), SerializeError> {
    // 속성 순서 (BorderFillType.cpp:64-68): id, threeD, shadow, centerLine, breakCellSeparateLine
    start_tag_attrs(
        w,
        "hh:borderFill",
        &[
            ("id", &(id + 1).to_string()), // HWPX 관찰: id는 1-based
            ("threeD", "0"),
            ("shadow", "0"),
            ("centerLine", "NONE"),
            ("breakCellSeparateLine", "0"),
        ],
    )?;

    // 자식 순서 (BorderFillType.cpp:51-58):
    // slash, backSlash, leftBorder, rightBorder, topBorder, bottomBorder, diagonal, fillBrush
    write_diag_line(
        w,
        "hh:slash",
        diagonal_shape_type(((bf.attr >> 2) & 0x07) as u8),
    )?;
    write_diag_line(
        w,
        "hh:backSlash",
        diagonal_shape_type(((bf.attr >> 5) & 0x07) as u8),
    )?;
    write_border_line(w, "hh:leftBorder", &bf.borders[0])?;
    write_border_line(w, "hh:rightBorder", &bf.borders[1])?;
    write_border_line(w, "hh:topBorder", &bf.borders[2])?;
    write_border_line(w, "hh:bottomBorder", &bf.borders[3])?;
    write_diagonal(w, &bf.diagonal)?;

    // fillBrush: Fill이 존재할 때만
    if !matches!(bf.fill.fill_type, FillType::None) {
        start_tag(w, "hc:fillBrush")?;
        if matches!(bf.fill.fill_type, FillType::Solid) {
            if let Some(solid) = bf.fill.solid {
                write_solid_fill(w, &solid, bf.fill.alpha)?;
            }
        } else if matches!(bf.fill.fill_type, FillType::Gradient) {
            if let Some(gradient) = bf.fill.gradient.as_ref() {
                write_gradient_fill(w, gradient, bf.fill.alpha)?;
            }
        }
        end_tag(w, "hc:fillBrush")?;
    }

    end_tag(w, "hh:borderFill")?;
    Ok(())
}

fn write_solid_fill<W: Write>(
    w: &mut Writer<W>,
    solid: &SolidFill,
    alpha: u8,
) -> Result<(), SerializeError> {
    let face_color = color_hex(solid.background_color);
    let hatch_color = color_hex(solid.pattern_color);
    let alpha_s;

    let mut attrs = vec![
        ("faceColor", face_color.as_str()),
        ("hatchColor", hatch_color.as_str()),
    ];
    if solid.pattern_type >= 0 {
        attrs.push(("hatchStyle", hatch_style_str(solid.pattern_type)));
    }
    if alpha != 0 {
        alpha_s = format!("{:.3}", f64::from(alpha) / 255.0);
        attrs.push(("alpha", alpha_s.as_str()));
    }

    empty_tag(w, "hc:winBrush", &attrs)
}

fn hatch_style_str(pattern_type: i32) -> &'static str {
    match pattern_type {
        1 => "HORIZONTAL",
        2 => "VERTICAL",
        3 => "BACK_SLASH",
        4 => "SLASH",
        5 => "CROSS",
        6 => "CROSS_DIAGONAL",
        _ => "HORIZONTAL",
    }
}

fn write_gradient_fill<W: Write>(
    w: &mut Writer<W>,
    gradient: &GradientFill,
    alpha: u8,
) -> Result<(), SerializeError> {
    let angle = gradient.angle.to_string();
    let center_x = gradient.center_x.to_string();
    let center_y = gradient.center_y.to_string();
    let blur = gradient.blur.to_string();
    let step_center = gradient.step_center.to_string();
    let alpha_s;
    let mut attrs = vec![
        ("type", gradient_type_str(gradient.gradient_type)),
        ("angle", angle.as_str()),
        ("centerX", center_x.as_str()),
        ("centerY", center_y.as_str()),
        ("blur", blur.as_str()),
        ("stepCenter", step_center.as_str()),
    ];
    if alpha != 0 {
        alpha_s = format!("{:.3}", f64::from(alpha) / 255.0);
        attrs.push(("alpha", alpha_s.as_str()));
    }

    start_tag_attrs(w, "hc:gradation", &attrs)?;
    for color in &gradient.colors {
        let value = color_hex(*color);
        empty_tag(w, "hc:color", &[("value", value.as_str())])?;
    }
    end_tag(w, "hc:gradation")
}

fn gradient_type_str(value: i16) -> &'static str {
    match value {
        1 => "LINEAR",
        2 => "RADIAL",
        3 => "CONICAL",
        4 => "SQUARE",
        _ => "LINEAR",
    }
}

fn write_diag_line<W: Write>(
    w: &mut Writer<W>,
    name: &str,
    type_str: &str,
) -> Result<(), SerializeError> {
    empty_tag(
        w,
        name,
        &[("type", type_str), ("Crooked", "0"), ("isCounter", "0")],
    )
}

fn diagonal_shape_type(code: u8) -> &'static str {
    match code & 0x07 {
        0 => "NONE",
        0b010 => "CENTER",
        0b011 => "CENTER_BELOW",
        0b110 => "CENTER_ABOVE",
        _ => "ALL",
    }
}

fn write_border_line<W: Write>(
    w: &mut Writer<W>,
    name: &str,
    line: &BorderLine,
) -> Result<(), SerializeError> {
    let type_str = border_line_type_str(line.line_type);
    let width_mm = format!("{} mm", border_width_mm(line.width));
    let color = color_hex(line.color);
    empty_tag(
        w,
        name,
        &[("type", type_str), ("width", &width_mm), ("color", &color)],
    )
}

fn write_diagonal<W: Write>(w: &mut Writer<W>, d: &DiagonalLine) -> Result<(), SerializeError> {
    let type_str = if d.width == 0 {
        "NONE"
    } else {
        diagonal_line_type_str(d.diagonal_type)
    };
    if d.width == 0 {
        return empty_tag(w, "hh:diagonal", &[("type", type_str)]);
    }

    let width_mm = format!("{} mm", border_width_mm(d.width));
    let color = color_hex(d.color);
    empty_tag(
        w,
        "hh:diagonal",
        &[("type", type_str), ("width", &width_mm), ("color", &color)],
    )
}

fn diagonal_line_type_str(value: u8) -> &'static str {
    match value {
        0 => "NONE",
        1 => "SOLID",
        2 => "DASH",
        3 => "DOT",
        4 => "DASH_DOT",
        5 => "DASH_DOT_DOT",
        6 => "LONG_DASH",
        7 => "CIRCLE",
        8 => "DOUBLE_SLIM",
        9 => "SLIM_THICK",
        10 => "THICK_SLIM",
        11 => "SLIM_THICK_SLIM",
        12 => "WAVE",
        13 => "DOUBLE_WAVE",
        _ => "SOLID",
    }
}

fn border_line_type_str(t: BorderLineType) -> &'static str {
    use BorderLineType::*;
    match t {
        None => "NONE",
        Solid => "SOLID",
        Dash => "DASH",
        Dot => "DOT",
        DashDot => "DASH_DOT",
        DashDotDot => "DASH_DOT_DOT",
        LongDash => "LONG_DASH",
        Circle => "CIRCLE",
        Double => "DOUBLE_SLIM",
        ThinThickDouble => "SLIM_THICK",
        ThickThinDouble => "THICK_SLIM",
        ThinThickThinTriple => "SLIM_THICK_SLIM",
        Wave => "WAVE",
        DoubleWave => "DOUBLE_WAVE",
        Thick3D => "THICK3D",
        Thick3DReverse => "THICKREV3D",
        Thin3D => "3D",
        Thin3DReverse => "REV3D",
    }
}

fn border_width_mm(w: u8) -> &'static str {
    // HWP 선 굵기 인덱스(0~) → mm (한컴 매핑)
    // 0=0.1mm, 1=0.12mm, 2=0.15mm, 3=0.2mm, 4=0.25mm, 5=0.3mm, 6=0.4mm, 7=0.5mm,
    // 8=0.6mm, 9=0.7mm, 10=1.0mm, 11=1.5mm, 12=2.0mm, 13=3.0mm, 14=4.0mm, 15=5.0mm
    // ref_empty.hwpx에서 기본값은 "0.1 mm" 관찰
    match w {
        0 => "0.1",
        1 => "0.12",
        2 => "0.15",
        3 => "0.2",
        4 => "0.25",
        5 => "0.3",
        6 => "0.4",
        7 => "0.5",
        8 => "0.6",
        9 => "0.7",
        10 => "1.0",
        11 => "1.5",
        12 => "2.0",
        13 => "3.0",
        14 => "4.0",
        15 => "5.0",
        _ => "0.1",
    }
}

fn color_hex(c: ColorRef) -> String {
    // ColorRef = u32. HWP 내부 저장: 상위 바이트가 비투명 플래그(0이면 유효 색상).
    // 0xFFFFFFFF = 투명/없음 센티넬 → "none"
    if c == 0xFFFFFFFF {
        return "none".to_string();
    }
    // HWPX는 "#RRGGBB" 또는 "#AARRGGBB".
    let a = ((c >> 24) & 0xFF) as u8;
    let r = (c & 0xFF) as u8;
    let g = ((c >> 8) & 0xFF) as u8;
    let b = ((c >> 16) & 0xFF) as u8;
    if a == 0 {
        format!("#{:02X}{:02X}{:02X}", r, g, b)
    } else {
        format!("#{:02X}{:02X}{:02X}{:02X}", a, r, g, b)
    }
}

// =====================================================================
// <hh:charProperties>
// =====================================================================
fn write_char_properties<W: Write>(
    w: &mut Writer<W>,
    doc_info: &DocInfo,
    ctx: &SerializeContext,
) -> Result<(), SerializeError> {
    let _ = ctx;
    if doc_info.char_shapes.is_empty() {
        return Ok(());
    }
    start_tag_attrs(
        w,
        "hh:charProperties",
        &[("itemCnt", &doc_info.char_shapes.len().to_string())],
    )?;
    for (idx, cs) in doc_info.char_shapes.iter().enumerate() {
        write_char_pr(w, idx as u32, cs)?;
    }
    end_tag(w, "hh:charProperties")?;
    Ok(())
}

fn write_char_pr<W: Write>(
    w: &mut Writer<W>,
    id: u32,
    cs: &CharShape,
) -> Result<(), SerializeError> {
    // 속성 순서 (CharShapeType.cpp:79-86): id, height, textColor, shadeColor,
    // useFontSpace, useKerning, symMark, borderFillIDRef
    let shade = if cs.shade_color == 0 {
        "none".to_string()
    } else {
        color_hex(cs.shade_color)
    };
    start_tag_attrs(
        w,
        "hh:charPr",
        &[
            ("id", &id.to_string()),
            ("height", &cs.base_size.to_string()),
            ("textColor", &color_hex(cs.text_color)),
            ("shadeColor", &shade),
            ("useFontSpace", bool01(false)),
            ("useKerning", bool01(cs.kerning)),
            ("symMark", sym_mark_str(cs.emphasis_dot)),
            ("borderFillIDRef", &cs.border_fill_id.to_string()),
        ],
    )?;

    // 자식 순서 (CharShapeType.cpp:59-73):
    // fontRef, ratio, spacing, relSz, offset, italic, bold, underline, strikeout, outline,
    // shadow, emboss, engrave, supscript, subscript
    write_lang_attrs(w, "hh:fontRef", &cs.font_ids.map(|v| v as i32))?;
    write_lang_attrs(w, "hh:ratio", &cs.ratios.map(|v| v as i32))?;
    write_lang_attrs(w, "hh:spacing", &cs.spacings.map(|v| v as i32))?;
    write_lang_attrs(w, "hh:relSz", &cs.relative_sizes.map(|v| v as i32))?;
    write_lang_attrs(w, "hh:offset", &cs.char_offsets.map(|v| v as i32))?;
    if cs.italic {
        empty_tag(w, "hh:italic", &[])?;
    }
    if cs.bold {
        empty_tag(w, "hh:bold", &[])?;
    }
    if !matches!(cs.underline_type, crate::model::style::UnderlineType::None)
        || cs.underline_color != 0
        || cs.underline_shape != 0
    {
        empty_tag(
            w,
            "hh:underline",
            &[
                ("type", underline_type_str(cs.underline_type)),
                ("shape", line_shape_str(cs.underline_shape)),
                ("color", &color_hex(cs.underline_color)),
            ],
        )?;
    }
    if cs.strikethrough || cs.strike_color != 0 || cs.strike_shape != 0 {
        empty_tag(
            w,
            "hh:strikeout",
            &[
                ("shape", strike_shape_str(cs)),
                ("color", &color_hex(cs.strike_color)),
            ],
        )?;
    }
    if cs.outline_type != 0 {
        empty_tag(
            w,
            "hh:outline",
            &[("type", outline_type_str(cs.outline_type))],
        )?;
    }
    if cs.shadow_type != 0
        || cs.shadow_color != 0
        || cs.shadow_offset_x != 0
        || cs.shadow_offset_y != 0
    {
        let shadow_type = if cs.shadow_type == 0 {
            "NONE"
        } else {
            "CONTINUOUS"
        };
        empty_tag(
            w,
            "hh:shadow",
            &[
                ("type", shadow_type),
                ("color", &color_hex(cs.shadow_color)),
                ("offsetX", &cs.shadow_offset_x.to_string()),
                ("offsetY", &cs.shadow_offset_y.to_string()),
            ],
        )?;
    }
    if cs.emboss {
        empty_tag(w, "hh:emboss", &[])?;
    }
    if cs.engrave {
        empty_tag(w, "hh:engrave", &[])?;
    }
    if cs.superscript {
        empty_tag(w, "hh:supscript", &[])?;
    }
    if cs.subscript {
        empty_tag(w, "hh:subscript", &[])?;
    }

    for switch_xml in &cs.hwpx_char_pr_switches {
        write_preserved_hwpx_switch(w, "charPr", switch_xml)?;
    }

    end_tag(w, "hh:charPr")?;
    Ok(())
}

fn write_lang_attrs<W: Write>(
    w: &mut Writer<W>,
    name: &str,
    vals: &[i32; 7],
) -> Result<(), SerializeError> {
    let s0 = vals[0].to_string();
    let s1 = vals[1].to_string();
    let s2 = vals[2].to_string();
    let s3 = vals[3].to_string();
    let s4 = vals[4].to_string();
    let s5 = vals[5].to_string();
    let s6 = vals[6].to_string();
    empty_tag(
        w,
        name,
        &[
            ("hangul", &s0),
            ("latin", &s1),
            ("hanja", &s2),
            ("japanese", &s3),
            ("other", &s4),
            ("symbol", &s5),
            ("user", &s6),
        ],
    )
}

fn bool01(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

fn sym_mark_str(em: u8) -> &'static str {
    match em {
        0 => "NONE",
        1 => "DOT_ABOVE",
        2 => "RING_ABOVE",
        3 => "TILDE",
        4 => "CARON",
        5 => "SIDE",
        6 => "COLON",
        _ => "NONE",
    }
}

fn underline_type_str(t: crate::model::style::UnderlineType) -> &'static str {
    use crate::model::style::UnderlineType::*;
    match t {
        None => "NONE",
        Bottom => "BOTTOM",
        Top => "TOP",
    }
}

fn line_shape_str(s: u8) -> &'static str {
    match s {
        0 => "SOLID",
        1 => "DASH",
        2 => "DOT",
        3 => "DASH_DOT",
        4 => "DASH_DOT_DOT",
        5 => "LONG_DASH",
        6 => "CIRCLE",
        7 => "DOUBLE_SLIM",
        8 => "SLIM_THICK",
        9 => "THICK_SLIM",
        10 => "SLIM_THICK_SLIM",
        11 => "WAVE",
        12 => "DOUBLE_WAVE",
        _ => "SOLID",
    }
}

fn strike_shape_str(cs: &CharShape) -> &'static str {
    if cs.strikethrough {
        line_shape_str(cs.strike_shape)
    } else {
        "NONE"
    }
}

fn outline_type_str(t: u8) -> &'static str {
    match t {
        0 => "NONE",
        1 => "SOLID",
        2 => "DASH",
        3 => "DOT",
        _ => "NONE",
    }
}

// =====================================================================
// <hh:tabProperties>
// =====================================================================
fn write_tab_properties<W: Write>(
    w: &mut Writer<W>,
    doc_info: &DocInfo,
) -> Result<(), SerializeError> {
    if doc_info.tab_defs.is_empty() {
        return Ok(());
    }
    start_tag_attrs(
        w,
        "hh:tabProperties",
        &[("itemCnt", &doc_info.tab_defs.len().to_string())],
    )?;
    for (idx, td) in doc_info.tab_defs.iter().enumerate() {
        write_tab_pr(w, idx as u16, td)?;
    }
    end_tag(w, "hh:tabProperties")?;
    Ok(())
}

fn write_tab_pr<W: Write>(w: &mut Writer<W>, id: u16, td: &TabDef) -> Result<(), SerializeError> {
    let attrs = [
        ("id", id.to_string()),
        ("autoTabLeft", bool01(td.auto_tab_left).to_string()),
        ("autoTabRight", bool01(td.auto_tab_right).to_string()),
    ];
    let attrs_ref: Vec<(&str, &str)> = attrs.iter().map(|(k, v)| (*k, v.as_str())).collect();

    if td.tabs.is_empty() && td.hwpx_tab_pr_switches.is_empty() {
        empty_tag(w, "hh:tabPr", &attrs_ref)?;
    } else {
        start_tag_attrs(w, "hh:tabPr", &attrs_ref)?;
        if td.hwpx_tab_pr_switches.is_empty() {
            for tab in &td.tabs {
                empty_tag(
                    w,
                    "hh:tabItem",
                    &[
                        ("pos", &tab.position.to_string()),
                        ("type", tab_type_str(tab.tab_type)),
                        ("leader", tab_leader_str(tab.fill_type)),
                    ],
                )?;
            }
        } else {
            for switch_xml in &td.hwpx_tab_pr_switches {
                write_preserved_hwpx_switch(w, "tabPr", switch_xml)?;
            }
        }
        end_tag(w, "hh:tabPr")?;
    }
    Ok(())
}

fn tab_type_str(t: u8) -> &'static str {
    match t {
        0 => "LEFT",
        1 => "RIGHT",
        2 => "CENTER",
        3 => "DECIMAL",
        _ => "LEFT",
    }
}

fn tab_leader_str(f: u8) -> &'static str {
    match f {
        0 => "NONE",
        1 => "SOLID",
        2 => "DOT",
        3 => "DASH",
        4 => "DASH_DOT",
        5 => "DASH_DOT_DOT",
        6 => "LONG_DASH",
        7 => "CIRCLE",
        8 => "DOUBLE_SLIM",
        _ => "NONE",
    }
}

// =====================================================================
// <hh:numberings>
// =====================================================================
fn write_numberings<W: Write>(w: &mut Writer<W>, doc_info: &DocInfo) -> Result<(), SerializeError> {
    if doc_info.numberings.is_empty() {
        return Ok(());
    }
    start_tag_attrs(
        w,
        "hh:numberings",
        &[("itemCnt", &doc_info.numberings.len().to_string())],
    )?;
    for (idx, n) in doc_info.numberings.iter().enumerate() {
        write_numbering(w, idx as u16, n)?;
    }
    end_tag(w, "hh:numberings")?;
    Ok(())
}

fn write_numbering<W: Write>(
    w: &mut Writer<W>,
    id: u16,
    n: &Numbering,
) -> Result<(), SerializeError> {
    start_tag_attrs(
        w,
        "hh:numbering",
        &[
            ("id", &(id + 1).to_string()), // 관찰: 1-based
            ("start", &n.start_number.to_string()),
        ],
    )?;
    // Stage 1: 10 레벨 paraHead 뼈대 출력. 실제 값은 NumberingHead 참조해 생성.
    for level in 0..10usize {
        let idx = level.min(6);
        let h = &n.heads[idx];
        let start = n.level_start_numbers.get(idx).copied().unwrap_or(1);
        let level_s = (level + 1).to_string();
        let start_s = start.to_string();
        let wa = h.width_adjust.to_string();
        let text_offset = h.text_distance.to_string();
        let char_pr_id = h.char_shape_id.to_string();
        let format_text = n.level_formats.get(idx).map(String::as_str).unwrap_or("");
        let num_format = numbering_format_str(h.number_format);
        let mut attrs = vec![
            ("start", start_s.as_str()),
            ("level", level_s.as_str()),
            ("align", "LEFT"),
            ("useInstWidth", "1"),
            ("autoIndent", "1"),
            ("widthAdjust", wa.as_str()),
            ("textOffsetType", "PERCENT"),
            ("textOffset", text_offset.as_str()),
            ("numFormat", num_format),
            ("charPrIDRef", char_pr_id.as_str()),
            ("checkable", "0"),
        ];
        if !format_text.is_empty() {
            attrs.push(("text", format_text));
        }
        empty_tag(w, "hh:paraHead", &attrs)?;
    }
    end_tag(w, "hh:numbering")?;
    Ok(())
}

fn numbering_format_str(value: u8) -> &'static str {
    match value {
        0 => "DIGIT",
        1 => "CIRCLED_DIGIT",
        2 => "ROMAN_CAPITAL",
        3 => "ROMAN_SMALL",
        4 => "LATIN_CAPITAL",
        5 => "LATIN_SMALL",
        8 => "HANGUL_SYLLABLE",
        12 => "HANGUL_NUMBER",
        13 => "HANJA_NUMBER",
        _ => "DIGIT",
    }
}

// =====================================================================
// <hh:paraProperties>
// =====================================================================
fn write_para_properties<W: Write>(
    w: &mut Writer<W>,
    doc_info: &DocInfo,
    ctx: &SerializeContext,
) -> Result<(), SerializeError> {
    let _ = ctx;
    if doc_info.para_shapes.is_empty() {
        return Ok(());
    }
    start_tag_attrs(
        w,
        "hh:paraProperties",
        &[("itemCnt", &doc_info.para_shapes.len().to_string())],
    )?;
    for (idx, ps) in doc_info.para_shapes.iter().enumerate() {
        write_para_pr(w, idx as u16, ps)?;
    }
    end_tag(w, "hh:paraProperties")?;
    Ok(())
}

fn write_para_pr<W: Write>(
    w: &mut Writer<W>,
    id: u16,
    ps: &ParaShape,
) -> Result<(), SerializeError> {
    let id_str = id.to_string();
    let tab_pr_id_ref = ps.tab_def_id.to_string();
    let condense = (((ps.attr1 >> 9) & 0x7f).min(75)).to_string();
    let font_line_height = bool_attr(ps.attr1 & (1 << 22) != 0);
    let snap_to_grid = bool_attr(ps.attr1 & (1 << 8) != 0);
    let break_non_latin_word = if ps.attr1 & (1 << 7) != 0 {
        "KEEP_WORD"
    } else {
        "BREAK_WORD"
    };
    let widow_orphan = bool_attr((ps.attr1 & (1 << 16) != 0) || (ps.attr2 & (1 << 5) != 0));
    let keep_with_next = bool_attr((ps.attr1 & (1 << 17) != 0) || (ps.attr2 & (1 << 6) != 0));
    let keep_lines = bool_attr((ps.attr1 & (1 << 18) != 0) || (ps.attr2 & (1 << 7) != 0));
    let page_break_before = bool_attr((ps.attr1 & (1 << 19) != 0) || (ps.attr2 & (1 << 8) != 0));

    // 속성 순서 (ParaShapeType.cpp:62-68): id, tabPrIDRef, condense,
    // fontLineHeight, snapToGrid, suppressLineNumbers, checked
    start_tag_attrs(
        w,
        "hh:paraPr",
        &[
            ("id", &id_str),
            ("tabPrIDRef", &tab_pr_id_ref),
            ("condense", &condense),
            ("fontLineHeight", font_line_height),
            ("snapToGrid", snap_to_grid),
            ("suppressLineNumbers", "0"),
            ("checked", "0"),
        ],
    )?;

    // 자식 순서 (ParaShapeType.cpp:50-56):
    // align, heading, breakSetting, margin, lineSpacing, border, autoSpacing
    empty_tag(
        w,
        "hh:align",
        &[
            ("horizontal", alignment_str(ps.alignment)),
            ("vertical", vertical_alignment_str((ps.attr1 >> 20) & 0x03)),
        ],
    )?;
    empty_tag(
        w,
        "hh:heading",
        &[
            ("type", head_type_str(ps.head_type)),
            ("idRef", &ps.numbering_id.to_string()),
            ("level", &ps.para_level.to_string()),
        ],
    )?;
    empty_tag(
        w,
        "hh:breakSetting",
        &[
            ("breakLatinWord", "KEEP_WORD"),
            ("breakNonLatinWord", break_non_latin_word),
            ("widowOrphan", widow_orphan),
            ("keepWithNext", keep_with_next),
            ("keepLines", keep_lines),
            ("pageBreakBefore", page_break_before),
            ("lineWrap", "BREAK"),
        ],
    )?;

    if ps.hwpx_para_pr_switches.is_empty() {
        // <hh:margin>: 자식 4개 (intent, left, right, prev, next) — 단위/값 지정
        super::utils::start_tag(w, "hh:margin")?;
        write_margin_child(w, "hh:intent", ps.indent)?;
        write_margin_child(w, "hh:left", ps.margin_left)?;
        write_margin_child(w, "hh:right", ps.margin_right)?;
        write_margin_child(w, "hh:prev", ps.spacing_before)?;
        write_margin_child(w, "hh:next", ps.spacing_after)?;
        end_tag(w, "hh:margin")?;

        empty_tag(
            w,
            "hh:lineSpacing",
            &[
                ("type", line_spacing_type_str(ps.line_spacing_type)),
                ("value", &ps.line_spacing.to_string()),
                ("unit", "HWPUNIT"),
            ],
        )?;
    } else {
        for switch_xml in &ps.hwpx_para_pr_switches {
            write_preserved_hwpx_switch(w, "paraPr", switch_xml)?;
        }
    }

    empty_tag(
        w,
        "hh:border",
        &[
            ("borderFillIDRef", &ps.border_fill_id.to_string()),
            ("offsetLeft", &ps.border_spacing[0].to_string()),
            ("offsetRight", &ps.border_spacing[1].to_string()),
            ("offsetTop", &ps.border_spacing[2].to_string()),
            ("offsetBottom", &ps.border_spacing[3].to_string()),
            ("connect", bool_attr(ps.attr1 & (1 << 28) != 0)),
            ("ignoreMargin", bool_attr(ps.attr1 & (1 << 29) != 0)),
        ],
    )?;

    empty_tag(
        w,
        "hh:autoSpacing",
        &[("eAsianEng", "0"), ("eAsianNum", "0")],
    )?;

    end_tag(w, "hh:paraPr")?;
    Ok(())
}

fn bool_attr(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn write_preserved_hwpx_switch<W: Write>(
    w: &mut Writer<W>,
    owner: &str,
    switch_xml: &str,
) -> Result<(), SerializeError> {
    w.get_mut()
        .write_all(switch_xml.as_bytes())
        .map_err(|e| SerializeError::XmlError(format!("{} switch preserve: {}", owner, e)))
}

fn write_margin_child<W: Write>(
    w: &mut Writer<W>,
    name: &str,
    value: i32,
) -> Result<(), SerializeError> {
    empty_tag(
        w,
        name,
        &[("unit", "HWPUNIT"), ("value", &value.to_string())],
    )
}

fn alignment_str(a: Alignment) -> &'static str {
    use Alignment::*;
    match a {
        Justify => "JUSTIFY",
        Left => "LEFT",
        Right => "RIGHT",
        Center => "CENTER",
        Distribute => "DISTRIBUTE",
        Split => "DISTRIBUTE_SPACE",
    }
}

fn vertical_alignment_str(value: u32) -> &'static str {
    match value & 0x03 {
        1 => "TOP",
        2 => "CENTER",
        3 => "BOTTOM",
        _ => "BASELINE",
    }
}

fn head_type_str(h: HeadType) -> &'static str {
    use HeadType::*;
    match h {
        None => "NONE",
        Outline => "OUTLINE",
        Number => "NUMBER",
        Bullet => "BULLET",
    }
}

fn line_spacing_type_str(t: LineSpacingType) -> &'static str {
    use LineSpacingType::*;
    match t {
        Percent => "PERCENT",
        Fixed => "FIXED",
        SpaceOnly => "BETWEEN_LINES",
        Minimum => "AT_LEAST",
    }
}

// =====================================================================
// <hh:styles>
// =====================================================================
fn write_styles<W: Write>(
    w: &mut Writer<W>,
    doc_info: &DocInfo,
    ctx: &SerializeContext,
) -> Result<(), SerializeError> {
    let _ = ctx;
    if doc_info.styles.is_empty() {
        return Ok(());
    }
    start_tag_attrs(
        w,
        "hh:styles",
        &[("itemCnt", &doc_info.styles.len().to_string())],
    )?;
    for (idx, st) in doc_info.styles.iter().enumerate() {
        write_style(w, idx as u16, st)?;
    }
    end_tag(w, "hh:styles")?;
    Ok(())
}

fn write_style<W: Write>(w: &mut Writer<W>, id: u16, st: &Style) -> Result<(), SerializeError> {
    let type_str = if st.style_type == 1 { "CHAR" } else { "PARA" };
    empty_tag(
        w,
        "hh:style",
        &[
            ("id", &id.to_string()),
            ("type", type_str),
            ("name", &st.local_name),
            ("engName", &st.english_name),
            ("paraPrIDRef", &st.para_shape_id.to_string()),
            ("charPrIDRef", &st.char_shape_id.to_string()),
            ("nextStyleIDRef", &st.next_style_id.to_string()),
            ("langID", "1042"),
            ("lockForm", "0"),
        ],
    )
}

// =====================================================================
// <hh:compatibleDocument>, <hh:docOption>, <hh:trackchageConfig>
// =====================================================================
fn write_compatible_document<W: Write>(w: &mut Writer<W>) -> Result<(), SerializeError> {
    start_tag_attrs(w, "hh:compatibleDocument", &[("targetProgram", "HWP201X")])?;
    super::utils::start_tag(w, "hh:layoutCompatibility")?;
    empty_tag(w, "hh:char", &[])?;
    empty_tag(w, "hh:paragraph", &[])?;
    empty_tag(w, "hh:section", &[])?;
    empty_tag(w, "hh:object", &[])?;
    empty_tag(w, "hh:field", &[])?;
    end_tag(w, "hh:layoutCompatibility")?;
    end_tag(w, "hh:compatibleDocument")?;
    Ok(())
}

fn write_doc_option<W: Write>(w: &mut Writer<W>) -> Result<(), SerializeError> {
    super::utils::start_tag(w, "hh:docOption")?;
    empty_tag(
        w,
        "hh:linkinfo",
        &[("path", ""), ("pageInherit", "0"), ("footnoteInherit", "0")],
    )?;
    end_tag(w, "hh:docOption")?;
    Ok(())
}

fn write_track_change_config<W: Write>(w: &mut Writer<W>) -> Result<(), SerializeError> {
    empty_tag(w, "hh:trackchageConfig", &[("flags", "0")])
}

// 내부에서 쓰는 start_tag 별명
use super::utils::start_tag;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::hwpx::header::parse_hwpx_header;
    use crate::parser::hwpx::parse_hwpx;

    #[test]
    fn write_header_runs_on_empty_document() {
        let doc = Document::default();
        let ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_header(&doc, &ctx).expect("write_header");
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(xml.contains("<hh:head"));
        assert!(xml.contains("</hh:head>"));
    }

    #[test]
    fn write_para_pr_preserves_clear_layout_bits() {
        let ps = ParaShape {
            attr1: 0,
            attr2: 0,
            ..ParaShape::default()
        };
        let mut writer = Writer::new(Vec::new());

        write_para_pr(&mut writer, 0, &ps).expect("write paraPr");
        let xml = String::from_utf8(writer.into_inner()).unwrap();

        assert!(
            xml.contains(r#"condense="0" fontLineHeight="0" snapToGrid="0""#),
            "paraPr attributes must follow ParaShape attr1 bits: {xml}"
        );
        assert!(
            xml.contains(r#"breakNonLatinWord="BREAK_WORD""#),
            "non-Latin break unit must follow ParaShape attr1 bit 7: {xml}"
        );
        assert!(
            xml.contains(r#"widowOrphan="0" keepWithNext="0" keepLines="0" pageBreakBefore="0""#),
            "break flags must follow ParaShape attr bits: {xml}"
        );
    }

    #[test]
    fn write_para_pr_preserves_set_layout_bits() {
        let ps = ParaShape {
            attr1: (30 << 9) | (1 << 22) | (1 << 8) | (1 << 7) | (1 << 16),
            attr2: (1 << 6) | (1 << 7) | (1 << 8),
            ..ParaShape::default()
        };
        let mut writer = Writer::new(Vec::new());

        write_para_pr(&mut writer, 3, &ps).expect("write paraPr");
        let xml = String::from_utf8(writer.into_inner()).unwrap();

        assert!(
            xml.contains(
                r#"id="3" tabPrIDRef="0" condense="30" fontLineHeight="1" snapToGrid="1""#
            ),
            "paraPr scalar attributes must follow ParaShape attr1 bits: {xml}"
        );
        assert!(
            xml.contains(r#"breakNonLatinWord="KEEP_WORD""#),
            "non-Latin break unit must follow ParaShape attr1 bit 7: {xml}"
        );
        assert!(
            xml.contains(r#"widowOrphan="1" keepWithNext="1" keepLines="1" pageBreakBefore="1""#),
            "break flags must follow ParaShape attr1/attr2 bits: {xml}"
        );
    }

    #[test]
    fn write_header_preserves_char_shape_count() {
        let bytes = include_bytes!("../../../samples/hwpx/ref/ref_empty.hwpx");
        let doc = parse_hwpx(bytes).expect("parse ref_empty");
        let ctx = SerializeContext::collect_from_document(&doc);
        let header_bytes = write_header(&doc, &ctx).expect("write header");
        let xml = std::str::from_utf8(&header_bytes).unwrap();
        // ref_empty.hwpx 의 charPr 개수는 관찰 결과 7개
        let expected = doc.doc_info.char_shapes.len();
        let actual = xml.matches("<hh:charPr ").count();
        assert_eq!(actual, expected, "charPr count mismatch");
    }

    #[test]
    fn write_char_pr_preserves_shadow_metadata_when_shadow_type_is_none() {
        let cs = CharShape {
            base_size: 1200,
            ratios: [100; 7],
            relative_sizes: [100; 7],
            shadow_type: 0,
            shadow_color: 0x00C0C0C0,
            shadow_offset_x: 10,
            shadow_offset_y: 10,
            ..CharShape::default()
        };
        let mut writer = Writer::new(Vec::new());

        write_char_pr(&mut writer, 0, &cs).expect("write charPr");
        let xml = String::from_utf8(writer.into_inner()).unwrap();

        assert!(
            xml.contains(r##"<hh:shadow type="NONE" color="#C0C0C0" offsetX="10" offsetY="10"/>"##),
            "shadow metadata with type NONE must survive charPr serialization: {xml}"
        );
    }

    #[test]
    fn write_header_emits_seven_fontfaces_when_populated() {
        let bytes = include_bytes!("../../../samples/hwpx/ref/ref_empty.hwpx");
        let doc = parse_hwpx(bytes).expect("parse");
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        assert_eq!(xml.matches("<hh:fontface ").count(), 7);
    }

    #[test]
    fn write_header_preserves_para_pr_switch_branch_structure() {
        let header_xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<hh:head xmlns:hh="http://www.hancom.co.kr/hwpml/2011/head"
  xmlns:hc="http://www.hancom.co.kr/hwpml/2011/core"
  xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph">
  <hh:refList>
    <hh:paraProperties itemCnt="1">
      <hh:paraPr id="0" tabPrIDRef="0" condense="0" fontLineHeight="0">
        <hh:align horizontal="JUSTIFY" vertical="BASELINE"/>
        <hh:margin>
          <hc:intent value="0" unit="HWPUNIT"/>
          <hc:left value="0" unit="HWPUNIT"/>
          <hc:right value="0" unit="HWPUNIT"/>
          <hc:prev value="0" unit="HWPUNIT"/>
          <hc:next value="0" unit="HWPUNIT"/>
        </hh:margin>
        <hp:switch>
          <hp:case required-namespace="http://www.hancom.co.kr/hwpml/2016/HwpUnitChar">
            <hh:margin>
              <hc:intent value="0" unit="HWPUNIT"/>
              <hc:left value="120" unit="HWPUNIT"/>
              <hc:right value="120" unit="HWPUNIT"/>
              <hc:prev value="0" unit="HWPUNIT"/>
              <hc:next value="0" unit="HWPUNIT"/>
            </hh:margin>
            <hh:lineSpacing type="PERCENT" value="160" unit="HWPUNIT"/>
          </hp:case>
          <hp:default>
            <hh:margin>
              <hc:intent value="0" unit="HWPUNIT"/>
              <hc:left value="240" unit="HWPUNIT"/>
              <hc:right value="240" unit="HWPUNIT"/>
              <hc:prev value="0" unit="HWPUNIT"/>
              <hc:next value="0" unit="HWPUNIT"/>
            </hh:margin>
            <hh:lineSpacing type="PERCENT" value="180" unit="HWPUNIT"/>
          </hp:default>
        </hp:switch>
      </hh:paraPr>
    </hh:paraProperties>
  </hh:refList>
</hh:head>"##;

        let (doc_info, doc_properties) = crate::parser::hwpx::header::parse_hwpx_header(header_xml)
            .expect("parse header switch");
        let doc = Document {
            doc_info,
            doc_properties,
            ..Document::default()
        };
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();

        assert_eq!(xml.matches("<hh:paraPr ").count(), 1);
        assert_eq!(
            xml.matches("<hp:switch").count(),
            1,
            "paraPr switch branch must survive header roundtrip: {xml}"
        );
        assert_eq!(
            xml.matches("<hp:case").count(),
            1,
            "paraPr switch case must survive header roundtrip: {xml}"
        );
        assert_eq!(
            xml.matches("<hp:default").count(),
            1,
            "paraPr switch default must survive header roundtrip: {xml}"
        );
        assert_eq!(
            xml.matches(r#"<hh:lineSpacing type="PERCENT" value="160" unit="HWPUNIT"/>"#)
                .count(),
            1,
            "paraPr switch case lineSpacing must survive header roundtrip: {xml}"
        );
        assert_eq!(
            xml.matches(r#"<hh:lineSpacing type="PERCENT" value="180" unit="HWPUNIT"/>"#)
                .count(),
            1,
            "paraPr switch default lineSpacing must survive header roundtrip: {xml}"
        );
    }

    #[test]
    fn write_header_preserves_tab_pr_switch_branch_structure() {
        let header_xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<hh:head xmlns:hh="http://www.hancom.co.kr/hwpml/2011/head"
  xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph">
  <hh:refList>
    <hh:tabProperties itemCnt="1">
      <hh:tabPr id="0" autoTabLeft="0" autoTabRight="0">
        <hp:switch>
          <hp:case required-namespace="http://www.hancom.co.kr/hwpml/2016/HwpUnitChar">
            <hh:tabItem pos="1200" type="LEFT" leader="NONE"/>
          </hp:case>
          <hp:default>
            <hh:tabItem pos="2400" type="LEFT" leader="NONE"/>
          </hp:default>
        </hp:switch>
      </hh:tabPr>
    </hh:tabProperties>
  </hh:refList>
</hh:head>"##;

        let (doc_info, doc_properties) =
            crate::parser::hwpx::header::parse_hwpx_header(header_xml).expect("parse tab switch");
        let doc = Document {
            doc_info,
            doc_properties,
            ..Document::default()
        };
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();

        assert_eq!(xml.matches("<hh:tabPr ").count(), 1);
        assert_eq!(xml.matches("<hp:switch").count(), 1, "{xml}");
        assert_eq!(xml.matches("<hp:case").count(), 1, "{xml}");
        assert_eq!(xml.matches("<hp:default").count(), 1, "{xml}");
        assert_eq!(
            xml.matches(r#"<hh:tabItem pos="1200" type="LEFT" leader="NONE"/>"#)
                .count(),
            1,
            "{xml}"
        );
        assert_eq!(
            xml.matches(r#"<hh:tabItem pos="2400" type="LEFT" leader="NONE"/>"#)
                .count(),
            1,
            "{xml}"
        );
    }

    #[test]
    fn canonical_attr_order_charpr() {
        let bytes = include_bytes!("../../../samples/hwpx/ref/ref_empty.hwpx");
        let doc = parse_hwpx(bytes).expect("parse");
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let snippet = xml
            .find("<hh:charPr ")
            .and_then(|i| {
                let end = xml[i..].find('>').map(|e| i + e)?;
                Some(&xml[i..=end])
            })
            .expect("charPr tag");
        // 속성이 id → height → textColor → shadeColor → useFontSpace → useKerning → symMark → borderFillIDRef 순서여야 함
        let ip = snippet.find("id=").unwrap();
        let hp = snippet.find("height=").unwrap();
        let tc = snippet.find("textColor=").unwrap();
        let sc = snippet.find("shadeColor=").unwrap();
        let uf = snippet.find("useFontSpace=").unwrap();
        let uk = snippet.find("useKerning=").unwrap();
        let sm = snippet.find("symMark=").unwrap();
        let bf = snippet.find("borderFillIDRef=").unwrap();
        assert!(ip < hp && hp < tc && tc < sc && sc < uf && uf < uk && uk < sm && sm < bf);
    }

    #[test]
    fn write_border_fill_preserves_slash_and_backslash_shape_types() {
        let mut bf = BorderFill::default();
        bf.attr = (0b010 << 2) | (0b011 << 5);

        let mut writer = Writer::new(Vec::new());
        write_border_fill(&mut writer, 0, &bf).expect("write borderFill");
        let xml = String::from_utf8(writer.into_inner()).unwrap();

        assert!(
            xml.contains(r#"<hh:slash type="CENTER" Crooked="0" isCounter="0"/>"#),
            "slash 방향 비트가 CENTER로 보존되어야 함: {xml}"
        );
        assert!(
            xml.contains(r#"<hh:backSlash type="CENTER_BELOW" Crooked="0" isCounter="0"/>"#),
            "backSlash 방향 비트가 CENTER_BELOW로 보존되어야 함: {xml}"
        );
    }

    #[test]
    fn write_header_preserves_font_type_info() {
        let mut doc = Document::default();
        doc.doc_info.font_faces = vec![
            vec![Font {
                name: "TestFont".to_string(),
                alt_type: 1,
                type_info: Some([2, 0, 5, 3, 2, 0, 0, 2, 0, 4]),
                ..Font::default()
            }],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        ];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        assert_eq!(
            doc_info.font_faces[0][0].type_info,
            Some([2, 0, 5, 3, 2, 0, 0, 2, 0, 4])
        );
    }

    #[test]
    fn write_header_preserves_solid_border_fill() {
        let mut bf = BorderFill::default();
        bf.fill.fill_type = FillType::Solid;
        bf.fill.solid = Some(SolidFill {
            background_color: 0x00F2F2F2,
            pattern_color: 0xFFFFFFFF,
            pattern_type: 1,
        });

        let mut doc = Document::default();
        doc.doc_info.border_fills = vec![bf];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        let actual = doc_info.border_fills[0].fill.solid.expect("solid fill");
        assert_eq!(doc_info.border_fills[0].fill.fill_type, FillType::Solid);
        assert_eq!(actual.background_color, 0x00F2F2F2);
        assert_eq!(actual.pattern_color, 0xFFFFFFFF);
        assert_eq!(actual.pattern_type, 1);
    }

    #[test]
    fn write_header_preserves_solid_hatch_color_without_hatch_style() {
        let mut bf = BorderFill::default();
        bf.fill.fill_type = FillType::Solid;
        bf.fill.solid = Some(SolidFill {
            background_color: 0x00D3F1E1,
            pattern_color: 0x00999999,
            pattern_type: -1,
        });

        let mut doc = Document::default();
        doc.doc_info.border_fills = vec![bf];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        let actual = doc_info.border_fills[0].fill.solid.expect("solid fill");
        assert_eq!(actual.background_color, 0x00D3F1E1);
        assert_eq!(actual.pattern_color, 0x00999999);
        assert_eq!(actual.pattern_type, -1);
    }

    #[test]
    fn write_header_preserves_empty_diagonal_width() {
        let mut bf = BorderFill::default();
        bf.diagonal.width = 0;

        let mut doc = Document::default();
        doc.doc_info.border_fills = vec![bf];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        assert_eq!(doc_info.border_fills[0].diagonal.width, 0);
    }

    #[test]
    fn write_header_preserves_diagonal_line_type() {
        let mut bf = BorderFill::default();
        bf.diagonal.diagonal_type = 8;
        bf.diagonal.width = 6;

        let mut doc = Document::default();
        doc.doc_info.border_fills = vec![bf];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        assert_eq!(doc_info.border_fills[0].diagonal.diagonal_type, 8);
        assert_eq!(doc_info.border_fills[0].diagonal.width, 6);
    }

    #[test]
    fn write_header_preserves_gradient_border_fill() {
        let mut bf = BorderFill::default();
        bf.fill.fill_type = FillType::Gradient;
        bf.fill.gradient = Some(GradientFill {
            gradient_type: 1,
            angle: 90,
            center_x: 0,
            center_y: 0,
            blur: 255,
            step_center: 50,
            colors: vec![0x00235CE3, 0x0049F279],
            positions: vec![],
        });

        let mut doc = Document::default();
        doc.doc_info.border_fills = vec![bf];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        let actual = doc_info.border_fills[0]
            .fill
            .gradient
            .as_ref()
            .expect("gradient fill");
        assert_eq!(doc_info.border_fills[0].fill.fill_type, FillType::Gradient);
        assert_eq!(actual.gradient_type, 1);
        assert_eq!(actual.angle, 90);
        assert_eq!(actual.blur, 255);
        assert_eq!(actual.step_center, 50);
        assert_eq!(actual.colors, vec![0x00235CE3, 0x0049F279]);
    }

    #[test]
    fn write_header_preserves_inactive_underline_and_strikeout_colors() {
        let mut doc = Document::default();
        doc.doc_info.char_shapes = vec![CharShape {
            underline_type: crate::model::style::UnderlineType::None,
            underline_color: 0x00FF0000,
            strikethrough: false,
            strike_color: 0x000000FF,
            ..CharShape::default()
        }];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");
        let actual = &doc_info.char_shapes[0];

        assert_eq!(
            actual.underline_type,
            crate::model::style::UnderlineType::None
        );
        assert_eq!(actual.underline_color, 0x00FF0000);
        assert!(!actual.strikethrough);
        assert_eq!(actual.strike_color, 0x000000FF);
    }

    #[test]
    fn write_header_preserves_para_border_flags() {
        let mut doc = Document::default();
        doc.doc_info.para_shapes = vec![ParaShape {
            attr1: (1 << 28) | (1 << 29),
            ..ParaShape::default()
        }];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        assert_eq!(doc_info.para_shapes[0].attr1 & (1 << 28), 1 << 28);
        assert_eq!(doc_info.para_shapes[0].attr1 & (1 << 29), 1 << 29);
    }

    #[test]
    fn write_header_preserves_para_vertical_alignment_bits() {
        let mut doc = Document::default();
        doc.doc_info.para_shapes = vec![ParaShape {
            attr1: 2 << 20,
            ..ParaShape::default()
        }];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");

        assert_eq!((doc_info.para_shapes[0].attr1 >> 20) & 0x03, 2);
    }

    #[test]
    fn write_header_preserves_numbering_head_fields() {
        let mut numbering = Numbering::default();
        numbering.start_number = 0;
        numbering.level_start_numbers[0] = 3;
        numbering.level_formats[0] = "^1.".to_string();
        numbering.heads[0] = NumberingHead {
            width_adjust: 800,
            text_distance: 35,
            char_shape_id: 20,
            number_format: 8,
            ..NumberingHead::default()
        };

        let mut doc = Document::default();
        doc.doc_info.numberings = vec![numbering];
        let ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_header(&doc, &ctx).unwrap()).unwrap();
        let (doc_info, _) = parse_hwpx_header(&xml).expect("parse serialized header");
        let actual = &doc_info.numberings[0];

        assert_eq!(actual.start_number, 0);
        assert_eq!(actual.level_start_numbers[0], 3);
        assert_eq!(actual.level_formats[0], "^1.");
        assert_eq!(actual.heads[0].width_adjust, 800);
        assert_eq!(actual.heads[0].text_distance, 35);
        assert_eq!(actual.heads[0].char_shape_id, 20);
        assert_eq!(actual.heads[0].number_format, 8);
    }

    #[test]
    fn diagonal_shape_type_matches_hwpx_parser_codes() {
        assert_eq!(diagonal_shape_type(0), "NONE");
        assert_eq!(diagonal_shape_type(0b010), "CENTER");
        assert_eq!(diagonal_shape_type(0b011), "CENTER_BELOW");
        assert_eq!(diagonal_shape_type(0b110), "CENTER_ABOVE");
        assert_eq!(diagonal_shape_type(0b111), "ALL");
    }
}
