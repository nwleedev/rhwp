//! Contents/section{N}.xml — Section 본문 직렬화
//!
//! Stage 2 (#182): 기존 템플릿 기반 구조를 유지하되, `<hp:p>` 와 `<hp:run>` 의 속성을
//! IR에서 가져와 동적으로 생성한다. `secPr`/`pagePr`/`grid` 등 섹션 정의는 템플릿 보존
//! (IR에 대응 필드가 더 담길 때까지 점진적으로 동적화 예정).
//!
//! Stage #177 (2026-04-18): `<hp:lineseg>` 직렬화를 IR 기반으로 전환.
//! `Paragraph.line_segs` 의 9개 필드(textpos, vertpos, vertsize, textheight, baseline,
//! spacing, horzpos, horzsize, flags)를 그대로 출력하여 **원본 lineseg 값 보존**.
//! rhwp 는 자신의 문서에서 새로 부정확한 값을 생산하지 않는다.
//!
//! IR 매핑 관행:
//!   - `section.paragraphs` 여러 개 = 하드 문단 경계 (`<hp:p>` 여러 개)
//!   - `paragraph.text` 내 `\n` = 소프트 라인브레이크 (`<hp:lineBreak/>`, 같은 문단 내)
//!   - `paragraph.text` 내 `\t` = 탭 (`<hp:tab width=... leader="0" type="1"/>`)
//!   - `paragraph.para_shape_id` → `<hp:p paraPrIDRef>`
//!   - `paragraph.style_id` → `<hp:p styleIDRef>`
//!   - `paragraph.column_type` → `<hp:p pageBreak/columnBreak>`
//!   - `paragraph.char_shapes` → text-only 문단의 `<hp:run charPrIDRef>` 구간
//!   - `paragraph.line_segs[i]` → 각 `<hp:lineseg>` 속성 (9개 필드 그대로 출력)

use quick_xml::Writer;

use crate::model::control::{
    AutoNumber, AutoNumberType, Bookmark, CharOverlap, Control, Equation, Markpen, NewNumber,
    PageHide, PageNumberPos,
};
use crate::model::document::{Document, Section};
use crate::model::footnote::{
    Endnote, Footnote, FootnoteNumbering, FootnotePlacement, FootnoteShape, NumberFormat,
};
use crate::model::header_footer::{Footer, Header, HeaderFooterApply};
use crate::model::page::{
    ColumnDef, ColumnDirection, ColumnType, PageBorderFill, PageBorderFillApply,
};
use crate::model::paragraph::{ColumnBreakType, LineSeg, Paragraph};
use crate::model::shape::{
    CommonObjAttr, HorzAlign, HorzRelTo, ShapeObject, TextWrap, VertAlign, VertRelTo,
};

use super::context::SerializeContext;
use super::field::{write_bookmark, write_field_begin, write_field_end};
use super::utils::xml_escape;
use super::SerializeError;
use super::{picture, table};

const EMPTY_SECTION_XML: &str = include_str!("templates/empty_section0.xml");
const LINESEG_SLOT_OPEN: &str = "<hp:linesegarray>";
const LINESEG_SLOT_CLOSE: &str = "</hp:linesegarray>";
const PARA_CLOSE: &str = "</hp:p></hs:sec>";

// 템플릿 내 첫 <hp:p> 태그의 실제 문자열 (id="3121190098" 랜덤 해시 포함).
// 템플릿은 정적이므로 이 문자열이 고정 위치에 있음이 보장됨.
const TEMPLATE_FIRST_P_TAG: &str = r#"<hp:p id="3121190098" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0">"#;
// 템플릿 내 본문 텍스트용 run. 앞의 secPr/colPr run과 구분되는 두 번째 run이다.
const TEMPLATE_TEXT_RUN: &str = r#"<hp:run charPrIDRef="0"><hp:t/></hp:run>"#;
const TEMPLATE_SECTION_RUN_OPEN: &str = r#"<hp:run charPrIDRef="0"><hp:secPr"#;
const TEMPLATE_START_NUM: &str =
    r#"<hp:startNum pageStartsOn="BOTH" page="0" pic="0" tbl="0" equation="0"/>"#;

/// 레퍼런스 기준 줄 레이아웃 파라미터.
const VERT_STEP: u32 = 1600; // vertsize(1000) + spacing(600)
/// 탭 기본 폭 (한컴이 열면서 재계산하지만 초기값으로 필요).
const TAB_DEFAULT_WIDTH: u32 = 4000;

/// Stage 2 진입점. `ctx` 는 Stage 3+ 에서 파라미터 검증에 사용.
pub fn write_section(
    section: &Section,
    _doc: &Document,
    _index: usize,
    ctx: &mut SerializeContext,
) -> Result<Vec<u8>, SerializeError> {
    let mut vert_cursor: u32 = 0;

    let first_para = section.paragraphs.first();
    let (first_body, first_linesegs, first_advance) = match first_para {
        Some(p) => render_first_paragraph_xml_parts(p, vert_cursor, ctx),
        None => {
            let (text, linesegs, advance) = render_paragraph_parts_for_text("", vert_cursor);
            (
                format!(r#"<hp:run charPrIDRef="0">{}</hp:run>"#, text),
                linesegs,
                advance,
            )
        }
    };
    vert_cursor = first_advance;

    let mut out = replace_first_linesegs(EMPTY_SECTION_XML, &first_linesegs);
    out = out.replacen(TEMPLATE_TEXT_RUN, &first_body, 1);
    out = replace_page_pr(&out, &section.section_def.page_def);
    out = replace_start_num(&out, &section.section_def);
    out = replace_note_prs(&out, &section.section_def);
    out = replace_page_border_fills(&out, &section.section_def);
    if let Some(p) = first_para {
        let first_char_shape_id =
            first_section_run_char_shape_id(p).unwrap_or_else(|| first_run_char_shape_id(p));
        if first_char_shape_id != 0 {
            out = replace_first_section_run_char_shape(&out, first_char_shape_id);
        }
    }

    if let Some(p) = first_para {
        let new_p_tag = render_hp_p_open_allocated(p, ctx);
        out = out.replacen(TEMPLATE_FIRST_P_TAG, &new_p_tag, 1);
    }

    // 추가 문단: `</hp:p></hs:sec>` 직전에 `<hp:p>` 요소를 삽입.
    if section.paragraphs.len() > 1 {
        let mut extra = String::new();
        for p in section.paragraphs.iter().skip(1) {
            let (runs, linesegs, advance) = render_paragraph_parts(p, vert_cursor, ctx);
            vert_cursor = advance;
            extra.push_str(&render_hp_p_open_allocated(p, ctx));
            extra.push_str(&runs);
            extra.push_str(&linesegs);
            extra.push_str("</hp:p>");
        }
        out = out.replacen(PARA_CLOSE, &format!("</hp:p>{}</hs:sec>", extra), 1);
    }

    Ok(out.into_bytes())
}

fn render_first_paragraph_xml_parts(
    para: &Paragraph,
    vert_start: u32,
    ctx: &mut SerializeContext,
) -> (String, String, u32) {
    let body_para = first_paragraph_body_without_template_section_prefix(para);
    let render_para = body_para.as_ref().unwrap_or(para);
    let runs_xml = render_paragraph_runs(render_para, ctx);

    if !para.line_segs.is_empty() {
        let linesegs = format!(
            "{}{}{}",
            LINESEG_SLOT_OPEN,
            render_lineseg_array_from_ir(&para.line_segs),
            LINESEG_SLOT_CLOSE
        );
        let vert_end = next_vert_cursor_from_ir(&para.line_segs, vert_start);
        (runs_xml, linesegs, vert_end)
    } else {
        (runs_xml, String::new(), vert_start)
    }
}

fn first_paragraph_body_without_template_section_prefix(para: &Paragraph) -> Option<Paragraph> {
    let prefix_count = first_section_prefix_control_count(para)?;
    let prefix_units = prefix_count as u32 * 8;
    if !para
        .hwpx_run_spans
        .first()
        .is_some_and(|span| span.start_pos == 0 && span.end_pos == prefix_units)
    {
        return None;
    }

    let mut filtered = para.clone();
    filtered.controls.drain(0..prefix_count);
    if filtered.ctrl_data_records.len() >= prefix_count {
        filtered.ctrl_data_records.drain(0..prefix_count);
    }
    filtered.char_count = filtered.char_count.saturating_sub(prefix_units);
    filtered.char_offsets = filtered
        .char_offsets
        .iter()
        .map(|offset| offset.saturating_sub(prefix_units))
        .collect();
    for char_shape in &mut filtered.char_shapes {
        char_shape.start_pos = char_shape.start_pos.saturating_sub(prefix_units);
    }
    filtered.hwpx_run_spans = filtered
        .hwpx_run_spans
        .into_iter()
        .skip(1)
        .map(|mut span| {
            span.start_pos = span.start_pos.saturating_sub(prefix_units);
            span.end_pos = span.end_pos.saturating_sub(prefix_units);
            span
        })
        .collect();
    for field_range in &mut filtered.field_ranges {
        field_range.control_idx = field_range.control_idx.saturating_sub(prefix_count);
    }

    Some(filtered)
}

fn first_section_prefix_control_count(para: &Paragraph) -> Option<usize> {
    if !matches!(para.controls.first(), Some(Control::SectionDef(_))) {
        return None;
    }

    let mut count = 1usize;
    if matches!(para.controls.get(1), Some(Control::ColumnDef(_))) {
        count += 1;
    }

    Some(count)
}

fn first_section_run_char_shape_id(para: &Paragraph) -> Option<u32> {
    first_section_prefix_control_count(para)?;
    para.hwpx_run_spans.first().map(|span| span.char_shape_id)
}

fn replace_first_section_run_char_shape(xml: &str, char_shape_id: u32) -> String {
    xml.replacen(
        TEMPLATE_SECTION_RUN_OPEN,
        &format!(r#"<hp:run charPrIDRef="{char_shape_id}"><hp:secPr"#),
        1,
    )
}

fn replace_start_num(xml: &str, section_def: &crate::model::document::SectionDef) -> String {
    xml.replacen(TEMPLATE_START_NUM, &render_start_num(section_def), 1)
}

fn render_start_num(section_def: &crate::model::document::SectionDef) -> String {
    format!(
        r#"<hp:startNum pageStartsOn="BOTH" page="{}" pic="{}" tbl="{}" equation="{}"/>"#,
        section_def.page_num,
        section_def.picture_num,
        section_def.table_num,
        section_def.equation_num,
    )
}

const TEMPLATE_FOOT_NOTE_PR: &str = concat!(
    r#"<hp:footNotePr>"#,
    r#"<hp:autoNumFormat type="DIGIT" userChar="" prefixChar="" suffixChar=")" supscript="0"/>"#,
    r##"<hp:noteLine length="-1" type="SOLID" width="0.12 mm" color="#000000"/>"##,
    r#"<hp:noteSpacing betweenNotes="283" belowLine="567" aboveLine="850"/>"#,
    r#"<hp:numbering type="CONTINUOUS" newNum="1"/>"#,
    r#"<hp:placement place="EACH_COLUMN" beneathText="0"/>"#,
    r#"</hp:footNotePr>"#,
);
const TEMPLATE_END_NOTE_PR: &str = concat!(
    r#"<hp:endNotePr>"#,
    r#"<hp:autoNumFormat type="DIGIT" userChar="" prefixChar="" suffixChar=")" supscript="0"/>"#,
    r##"<hp:noteLine length="14692344" type="SOLID" width="0.12 mm" color="#000000"/>"##,
    r#"<hp:noteSpacing betweenNotes="0" belowLine="567" aboveLine="850"/>"#,
    r#"<hp:numbering type="CONTINUOUS" newNum="1"/>"#,
    r#"<hp:placement place="END_OF_DOCUMENT" beneathText="0"/>"#,
    r#"</hp:endNotePr>"#,
);

fn replace_note_prs(xml: &str, section_def: &crate::model::document::SectionDef) -> String {
    let out = if xml.contains(TEMPLATE_FOOT_NOTE_PR) {
        xml.replacen(
            TEMPLATE_FOOT_NOTE_PR,
            &render_note_pr("footNotePr", &section_def.footnote_shape, false),
            1,
        )
    } else {
        xml.to_string()
    };
    if out.contains(TEMPLATE_END_NOTE_PR) {
        out.replacen(
            TEMPLATE_END_NOTE_PR,
            &render_note_pr("endNotePr", &section_def.endnote_shape, true),
            1,
        )
    } else {
        out
    }
}

fn render_note_pr(tag: &str, shape: &FootnoteShape, is_end_note: bool) -> String {
    format!(
        concat!(
            r#"<hp:{tag}>"#,
            r#"<hp:autoNumFormat type="{number_format}" userChar="{user_char}" prefixChar="{prefix_char}" suffixChar="{suffix_char}" supscript="0"/>"#,
            r#"<hp:noteLine length="{line_length}" type="{line_type}" width="{line_width}" color="{line_color}"/>"#,
            r#"<hp:noteSpacing betweenNotes="{between_notes}" belowLine="{below_line}" aboveLine="{above_line}"/>"#,
            r#"<hp:numbering type="{numbering}" newNum="{new_num}"/>"#,
            r#"<hp:placement place="{placement}" beneathText="0"/>"#,
            r#"</hp:{tag}>"#,
        ),
        tag = tag,
        number_format = note_number_format_to_hwpx(shape.number_format),
        user_char = ctrl_char_attr(shape.user_char),
        prefix_char = ctrl_char_attr(shape.prefix_char),
        suffix_char = ctrl_char_attr(shape.suffix_char),
        line_length = shape.separator_length,
        line_type = column_line_type_to_hwpx(shape.separator_line_type),
        line_width = column_line_width_to_hwpx(shape.separator_line_width),
        line_color = color_ref_to_hwpx(shape.separator_color),
        between_notes = shape.raw_unknown,
        below_line = shape.note_spacing,
        above_line = shape.separator_margin_bottom,
        numbering = note_numbering_to_hwpx(shape.numbering),
        new_num = shape.start_number,
        placement = note_placement_to_hwpx(shape.placement, is_end_note),
    )
}

fn note_number_format_to_hwpx(value: NumberFormat) -> &'static str {
    match value {
        NumberFormat::Digit => "DIGIT",
        NumberFormat::CircledDigit => "CIRCLE_DIGIT",
        NumberFormat::UpperRoman => "ROMAN_CAPITAL",
        NumberFormat::LowerRoman => "ROMAN_SMALL",
        NumberFormat::UpperAlpha => "LATIN_CAPITAL",
        NumberFormat::LowerAlpha => "LATIN_SMALL",
        NumberFormat::HangulSyllable => "HANGUL_SYLLABLE",
        NumberFormat::CircledHangulSyllable => "CIRCLED_HANGUL_SYLLABLE",
        NumberFormat::HangulJamo => "HANGUL_JAMO",
        NumberFormat::HanjaDigit => "HANJA",
        NumberFormat::UserChar => "USER_CHAR",
        _ => "DIGIT",
    }
}

fn note_numbering_to_hwpx(value: FootnoteNumbering) -> &'static str {
    match value {
        FootnoteNumbering::Continue => "CONTINUOUS",
        FootnoteNumbering::RestartSection => "ON_SECTION",
        FootnoteNumbering::RestartPage => "ON_PAGE",
    }
}

fn note_placement_to_hwpx(value: FootnotePlacement, is_end_note: bool) -> &'static str {
    match value {
        FootnotePlacement::EachColumn if is_end_note => "END_OF_DOCUMENT",
        FootnotePlacement::EachColumn => "EACH_COLUMN",
        FootnotePlacement::BelowText => "END_OF_SECTION",
        FootnotePlacement::RightColumn => "RIGHT_COLUMN",
    }
}

/// IR의 Paragraph를 기반으로 `<hp:p>` 시작 태그를 생성.
///
/// `id` 는 문단 순서 기반(0, 1, 2, ...)로 할당한다. 한컴 샘플은 랜덤 해시도 쓰지만
/// 파서는 id 를 무시하므로 순차값으로 충분.
pub(crate) fn render_hp_p_open_allocated(p: &Paragraph, ctx: &mut SerializeContext) -> String {
    let id = ctx.allocate_para_id(preserved_paragraph_id(p));
    render_hp_p_open(p, id)
}

pub(crate) fn render_hp_p_open(p: &Paragraph, id: u32) -> String {
    let page_break = if matches!(p.column_type, ColumnBreakType::Page) {
        1
    } else {
        0
    };
    let column_break = if matches!(p.column_type, ColumnBreakType::Column) {
        1
    } else {
        0
    };
    format!(
        r#"<hp:p id="{}" paraPrIDRef="{}" styleIDRef="{}" pageBreak="{}" columnBreak="{}" merged="0">"#,
        id, p.para_shape_id, p.style_id, page_break, column_break,
    )
}

fn preserved_paragraph_id(p: &Paragraph) -> Option<u32> {
    let bytes = p.raw_header_extra.get(6..10)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// 문단 첫 run 의 charPrIDRef. IR의 `char_shapes[0].char_shape_id` 사용.
/// 비어있으면 0 (기본 글자모양) 반환.
pub(crate) fn first_run_char_shape_id(p: &Paragraph) -> u32 {
    p.char_shapes.first().map(|r| r.char_shape_id).unwrap_or(0)
}

/// Paragraph 하나를 (완전한 `<hp:run>` 시퀀스 XML, `<hp:linesegarray>` 요소 XML,
/// 다음 vert_cursor)로 변환.
///
/// `<hp:lineseg>` 출력 원칙 (#177, #1380):
/// - `para.line_segs` 가 비어있지 않으면 **IR 값 그대로** `<hp:linesegarray>` 요소로 출력
/// - 비어있으면 **요소 자체를 방출 생략** (빈 문자열 반환) — 원본에 linesegarray 가
///   없는 문단의 보존 + rhwp 는 lineseg 를 새로 생산하지 않음. 한컴은 열 때 재계산
pub(crate) fn render_paragraph_parts(
    para: &Paragraph,
    vert_start: u32,
    ctx: &mut SerializeContext,
) -> (String, String, u32) {
    let runs_xml = render_paragraph_runs(para, ctx);

    if !para.line_segs.is_empty() {
        // IR 기반 출력 — 원본 lineseg 값 보존 (#177)
        let linesegs = format!(
            "{}{}{}",
            LINESEG_SLOT_OPEN,
            render_lineseg_array_from_ir(&para.line_segs),
            LINESEG_SLOT_CLOSE
        );
        let vert_end = next_vert_cursor_from_ir(&para.line_segs, vert_start);
        (runs_xml, linesegs, vert_end)
    } else {
        // IR 에 line_segs 없음 — linesegarray 방출 생략 (#1380)
        (runs_xml, String::new(), vert_start)
    }
}

/// Paragraph의 HWPX run 목록을 직렬화한다.
///
/// HWPX parser가 보존한 run span이 현재 text-only 문단과 일치하면 이를 우선 사용한다.
/// 그렇지 않으면 `Paragraph.char_shapes`에 남아 있는 서로 다른 character style segment를
/// semantic run으로 재구성한다.
pub(crate) fn render_paragraph_runs(para: &Paragraph, ctx: &mut SerializeContext) -> String {
    if can_render_hwpx_run_spans(para) {
        let runs = render_hwpx_run_spans(para, ctx);
        if !runs.is_empty() {
            return runs;
        }
    }

    render_runs(para, ctx)
}

fn can_render_hwpx_run_spans(para: &Paragraph) -> bool {
    if para.hwpx_run_spans.is_empty() {
        return false;
    }
    let slots = match hwpx_slots_with_positions(para) {
        Some(slots) => slots,
        None => return false,
    };
    if slots
        .iter()
        .any(|slot| matches!(slot.kind, HwpxRunSlotKind::Control(control) if !is_hwpx_inline_slot(control)))
    {
        return false;
    }

    let text_end = paragraph_text_utf16_len(para);
    let logical_end = para
        .char_count
        .checked_sub(1)
        .filter(|value| *value >= text_end)
        .unwrap_or_else(|| text_end.saturating_add(slots.len() as u32 * 8));
    let mut cursor = 0u32;
    for span in &para.hwpx_run_spans {
        if span.start_pos > span.end_pos || span.start_pos != cursor || span.end_pos > logical_end {
            return false;
        }
        cursor = span.end_pos;
    }

    cursor == logical_end
}

fn paragraph_text_utf16_len(para: &Paragraph) -> u32 {
    para.text.chars().map(char_utf16_width).sum()
}

fn render_hwpx_run_spans(para: &Paragraph, ctx: &mut SerializeContext) -> String {
    let mut out = String::new();
    let mut tab_idx = 0usize;
    let slots = hwpx_slots_with_positions(para);
    let mut slot_idx = 0usize;

    for span in &para.hwpx_run_spans {
        reference_char_shape_if_applicable(ctx, span.char_shape_id);
        out.push_str(&format!(r#"<hp:run charPrIDRef="{}">"#, span.char_shape_id));
        out.push_str(&render_hwpx_run_span_content(
            para,
            span,
            slots.as_deref(),
            &mut slot_idx,
            &mut tab_idx,
            ctx,
        ));
        out.push_str("</hp:run>");
    }

    out
}

enum HwpxRunSlotKind<'a> {
    Control(&'a Control),
    FieldEnd(u32),
}

struct HwpxRunSlot<'a> {
    pos: u32,
    kind: HwpxRunSlotKind<'a>,
}

fn hwpx_slots_with_positions(para: &Paragraph) -> Option<Vec<HwpxRunSlot<'_>>> {
    let slot_count = inferred_control_slot_count(para);
    let zero_width_slot_count = para.hwpx_zero_width_control_slots.len();
    if slot_count.saturating_add(zero_width_slot_count) != para.controls.len() {
        return None;
    }
    if !para.hwpx_zero_width_control_slots.iter().all(|slot| {
        matches!(
            para.controls.get(slot.control_idx),
            Some(Control::Bookmark(_) | Control::Markpen(_))
        )
    }) {
        return None;
    }

    let field_ends = field_end_slots_with_positions(para)?;
    let mut field_end_idx = 0usize;
    let mut control_idx = 0usize;
    let total_slot_count = para.controls.len() + field_ends.len();
    let mut slots = Vec::with_capacity(total_slot_count);
    let mut expected_utf16_pos = 0u32;
    for (idx, c) in para.text.chars().enumerate() {
        let char_pos = para
            .char_offsets
            .get(idx)
            .copied()
            .unwrap_or(expected_utf16_pos);
        push_hwpx_zero_width_slots_at_position(para, &mut control_idx, &mut slots, char_pos)?;
        if is_hwpx_auto_number_placeholder_at(
            para,
            idx,
            c,
            char_pos,
            expected_utf16_pos,
            control_idx,
        ) {
            slots.push(HwpxRunSlot {
                pos: char_pos,
                kind: HwpxRunSlotKind::Control(&para.controls[control_idx]),
            });
            control_idx += 1;
            expected_utf16_pos = char_pos.saturating_add(8);
            continue;
        }
        while slots.len() < total_slot_count && char_pos >= expected_utf16_pos.saturating_add(8) {
            push_hwpx_slot_at_position(
                para,
                &field_ends,
                &mut field_end_idx,
                &mut control_idx,
                &mut slots,
                expected_utf16_pos,
            )?;
            expected_utf16_pos = expected_utf16_pos.saturating_add(8);
        }
        push_hwpx_zero_width_slots_at_position(para, &mut control_idx, &mut slots, char_pos)?;
        let width = char_utf16_width(c);
        if char_pos >= expected_utf16_pos {
            expected_utf16_pos = char_pos.saturating_add(width);
        } else {
            expected_utf16_pos = expected_utf16_pos.saturating_add(width);
        }
    }
    let logical_end = para
        .char_count
        .checked_sub(1)
        .filter(|value| *value >= expected_utf16_pos)
        .unwrap_or_else(|| {
            expected_utf16_pos.saturating_add(
                (para.controls.len() + field_ends.len()).saturating_sub(slots.len()) as u32 * 8,
            )
        });
    push_hwpx_zero_width_slots_at_position(para, &mut control_idx, &mut slots, logical_end)?;
    while slots.len() < total_slot_count && expected_utf16_pos.saturating_add(8) <= logical_end {
        push_hwpx_slot_at_position(
            para,
            &field_ends,
            &mut field_end_idx,
            &mut control_idx,
            &mut slots,
            expected_utf16_pos,
        )?;
        expected_utf16_pos = expected_utf16_pos.saturating_add(8);
        push_hwpx_zero_width_slots_at_position(
            para,
            &mut control_idx,
            &mut slots,
            expected_utf16_pos,
        )?;
    }

    if control_idx == para.controls.len() && field_end_idx == field_ends.len() {
        slots.sort_by_key(|slot| slot.pos);
        Some(slots)
    } else {
        None
    }
}

fn push_hwpx_zero_width_slots_at_position<'a>(
    para: &'a Paragraph,
    control_idx: &mut usize,
    slots: &mut Vec<HwpxRunSlot<'a>>,
    pos: u32,
) -> Option<()> {
    while para
        .hwpx_zero_width_control_slots
        .iter()
        .any(|slot| slot.control_idx == *control_idx && slot.pos == pos)
    {
        let control = para.controls.get(*control_idx)?;
        slots.push(HwpxRunSlot {
            pos,
            kind: HwpxRunSlotKind::Control(control),
        });
        *control_idx += 1;
    }

    Some(())
}

fn push_hwpx_slot_at_position<'a>(
    para: &'a Paragraph,
    field_ends: &[(u32, u32)],
    field_end_idx: &mut usize,
    control_idx: &mut usize,
    slots: &mut Vec<HwpxRunSlot<'a>>,
    pos: u32,
) -> Option<()> {
    if field_ends
        .get(*field_end_idx)
        .is_some_and(|(field_end_pos, _)| *field_end_pos == pos)
    {
        let (_, field_id) = field_ends[*field_end_idx];
        slots.push(HwpxRunSlot {
            pos,
            kind: HwpxRunSlotKind::FieldEnd(field_id),
        });
        *field_end_idx += 1;
        return Some(());
    }

    let control = para.controls.get(*control_idx)?;
    slots.push(HwpxRunSlot {
        pos,
        kind: HwpxRunSlotKind::Control(control),
    });
    *control_idx += 1;
    Some(())
}

fn field_end_slots_with_positions(para: &Paragraph) -> Option<Vec<(u32, u32)>> {
    let mut positions = Vec::with_capacity(para.field_ranges.len());
    let text_len = para.text.chars().count();
    let logical_end = para.char_count.checked_sub(1)?;
    for range in &para.field_ranges {
        let field_id = match para.controls.get(range.control_idx) {
            Some(Control::Field(field)) => field.field_id,
            _ => return None,
        };
        let pos = if range.end_char_idx < text_len {
            para.char_offsets
                .get(range.end_char_idx)
                .copied()?
                .checked_sub(8)?
        } else {
            logical_end.checked_sub(8)?
        };
        positions.push((pos, field_id));
    }
    positions.sort_by_key(|(pos, _)| *pos);
    Some(positions)
}

fn is_hwpx_auto_number_placeholder_at(
    para: &Paragraph,
    char_idx: usize,
    c: char,
    char_pos: u32,
    expected_utf16_pos: u32,
    control_idx: usize,
) -> bool {
    if c != ' ' || char_pos < expected_utf16_pos {
        return false;
    }
    if !matches!(para.controls.get(control_idx), Some(Control::AutoNumber(_))) {
        return false;
    }
    let next_offset = para.char_offsets.get(char_idx + 1).copied();
    next_offset.map_or_else(
        || {
            para.char_count
                .checked_sub(1)
                .is_some_and(|end| end >= char_pos.saturating_add(8))
        },
        |next| next >= char_pos.saturating_add(8),
    )
}

fn render_hwpx_run_span_content(
    para: &Paragraph,
    span: &crate::model::paragraph::HwpxRunSpan,
    slots: Option<&[HwpxRunSlot<'_>]>,
    slot_idx: &mut usize,
    tab_idx: &mut usize,
    ctx: &mut SerializeContext,
) -> String {
    let mut out = String::new();
    let mut text_buf = String::new();
    let mut fallback_pos = 0u32;
    let start_pos = span.start_pos;
    let end_pos = span.end_pos;
    for (index, c) in para.text.chars().enumerate() {
        let char_pos = para
            .char_offsets
            .get(index)
            .copied()
            .unwrap_or(fallback_pos);
        if let Some(slots) = slots {
            if is_hwpx_auto_number_span_char(slots, *slot_idx, char_pos, c) {
                let slot = &slots[*slot_idx];
                if slot.pos < start_pos {
                    *slot_idx += 1;
                    fallback_pos = char_pos.saturating_add(8);
                    continue;
                }
                if slot.pos >= start_pos && slot.pos < end_pos {
                    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
                    render_hwpx_run_slot(&mut out, &slot.kind, ctx);
                    *slot_idx += 1;
                    fallback_pos = char_pos.saturating_add(8);
                    continue;
                }
            }
            while is_hwpx_zero_width_span_slot(slots, *slot_idx, char_pos) {
                let slot = &slots[*slot_idx];
                if slot.pos >= start_pos && slot.pos < end_pos {
                    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
                    render_hwpx_run_slot(&mut out, &slot.kind, ctx);
                    *slot_idx += 1;
                } else {
                    break;
                }
            }
        }
        if let Some(slots) = slots {
            while *slot_idx < slots.len() {
                let slot = &slots[*slot_idx];
                let slot_pos = slot.pos;
                if slot_pos < start_pos {
                    *slot_idx += 1;
                    continue;
                }
                if slot_pos >= end_pos || slot_pos >= char_pos {
                    break;
                }
                if slot_pos >= start_pos {
                    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
                    render_hwpx_run_slot(&mut out, &slot.kind, ctx);
                }
                *slot_idx += 1;
            }
        }
        let char_end = char_pos.saturating_add(char_utf16_width(c));
        fallback_pos = char_end;
        if char_pos >= start_pos && char_end <= end_pos {
            text_buf.push(c);
        }
    }
    if let Some(slots) = slots {
        if start_pos == end_pos {
            while *slot_idx < slots.len() && slots[*slot_idx].pos == start_pos {
                let slot = &slots[*slot_idx];
                if matches!(
                    slot.kind,
                    HwpxRunSlotKind::Control(Control::Bookmark(_) | Control::Markpen(_))
                ) {
                    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
                    render_hwpx_run_slot(&mut out, &slot.kind, ctx);
                    *slot_idx += 1;
                    continue;
                }
                break;
            }
        }
        while *slot_idx < slots.len() {
            let slot = &slots[*slot_idx];
            let slot_pos = slot.pos;
            let is_markpen_at_end = slot_pos == end_pos
                && matches!(slot.kind, HwpxRunSlotKind::Control(Control::Markpen(_)));
            if slot_pos >= end_pos && !is_markpen_at_end {
                break;
            }
            if slot_pos >= start_pos {
                flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
                render_hwpx_run_slot(&mut out, &slot.kind, ctx);
            }
            *slot_idx += 1;
        }
    }
    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
    for _ in 0..span.empty_t_count {
        out.push_str(&render_hp_t_content("", &para.tab_extended, tab_idx));
    }

    out
}

fn is_hwpx_auto_number_span_char(
    slots: &[HwpxRunSlot<'_>],
    slot_idx: usize,
    char_pos: u32,
    c: char,
) -> bool {
    c == ' '
        && slots.get(slot_idx).is_some_and(|slot| {
            slot.pos == char_pos
                && matches!(slot.kind, HwpxRunSlotKind::Control(Control::AutoNumber(_)))
        })
}

fn is_hwpx_zero_width_span_slot(slots: &[HwpxRunSlot<'_>], slot_idx: usize, char_pos: u32) -> bool {
    slots.get(slot_idx).is_some_and(|slot| {
        slot.pos == char_pos
            && matches!(
                slot.kind,
                HwpxRunSlotKind::Control(Control::Bookmark(_) | Control::Markpen(_))
            )
    })
}

fn render_hwpx_run_slot(out: &mut String, slot: &HwpxRunSlotKind<'_>, ctx: &mut SerializeContext) {
    match slot {
        HwpxRunSlotKind::Control(control) => render_control_slot(out, control, ctx),
        HwpxRunSlotKind::FieldEnd(field_id) => {
            if let Ok(xml) = writer_to_string(|w| write_field_end(w, *field_id)) {
                out.push_str("<hp:ctrl>");
                out.push_str(&xml);
                out.push_str("</hp:ctrl>");
            }
        }
    }
}

fn can_render_char_shape_runs(para: &Paragraph) -> bool {
    para.controls.is_empty() && para.field_ranges.is_empty() && para.char_shapes.len() > 1
}

fn render_text_char_shape_runs(para: &Paragraph, ctx: &mut SerializeContext) -> String {
    let mut shape_index = 0usize;
    let mut current_shape_id = para.char_shapes[0].char_shape_id;
    let mut expected_utf16_pos = 0u32;
    let mut segment_text = String::new();
    let mut tab_idx = 0usize;
    let mut out = String::new();

    for (char_index, c) in para.text.chars().enumerate() {
        let char_pos = para
            .char_offsets
            .get(char_index)
            .copied()
            .unwrap_or(expected_utf16_pos);

        while shape_index + 1 < para.char_shapes.len()
            && char_pos >= para.char_shapes[shape_index + 1].start_pos
        {
            flush_run_segment(
                &mut out,
                &mut segment_text,
                current_shape_id,
                &para.tab_extended,
                &mut tab_idx,
                ctx,
            );
            shape_index += 1;
            current_shape_id = para.char_shapes[shape_index].char_shape_id;
        }

        segment_text.push(c);
        expected_utf16_pos = char_pos.saturating_add(char_utf16_width(c));
    }

    flush_run_segment(
        &mut out,
        &mut segment_text,
        current_shape_id,
        &para.tab_extended,
        &mut tab_idx,
        ctx,
    );

    out
}

fn reference_char_shape_if_applicable(ctx: &mut SerializeContext, char_shape_id: u32) {
    if ctx.char_shape_ids.registered_count() > 0 || char_shape_id != 0 {
        ctx.char_shape_ids.reference(char_shape_id);
    }
}

fn flush_run_segment(
    out: &mut String,
    segment_text: &mut String,
    char_shape_id: u32,
    tab_extended: &[[u16; 7]],
    tab_idx: &mut usize,
    ctx: &mut SerializeContext,
) {
    if segment_text.is_empty() {
        return;
    }

    reference_char_shape_if_applicable(ctx, char_shape_id);
    out.push_str(&format!(r#"<hp:run charPrIDRef="{}">"#, char_shape_id));
    out.push_str(&render_hp_t_content(segment_text, tab_extended, tab_idx));
    out.push_str("</hp:run>");
    segment_text.clear();
}

/// IR 없이 텍스트만 있을 때 `<hp:t>` 와 fallback lineseg 생성.
/// `write_section` 이 `first_para == None` 인 경우를 위해 유지.
fn render_paragraph_parts_for_text(text: &str, vert_start: u32) -> (String, String, u32) {
    let t_xml = render_hp_t_content(text, &[], &mut 0);
    (t_xml, String::new(), vert_start)
}

/// `<hp:t>...</hp:t>` 본문 생성 — 탭/소프트브레이크/XML escape 포함.
///
/// `tab_extended`: IR의 탭 확장 정보 목록. `tab_idx`를 통해 탭 문자마다 순서대로 참조.
/// 항목이 없으면 폴백(width=TAB_DEFAULT_WIDTH, leader=0, type=1)을 사용.
pub(crate) fn render_hp_t_content(
    text: &str,
    tab_extended: &[[u16; 7]],
    tab_idx: &mut usize,
) -> String {
    let mut t_xml = String::from("<hp:t>");
    let mut buf = String::new();
    for c in text.chars() {
        match c {
            '\t' => {
                flush_buf(&mut t_xml, &mut buf);
                let (width, leader, tab_type) = if let Some(ext) = tab_extended.get(*tab_idx) {
                    *tab_idx += 1;
                    (ext[0] as u32, ext[2] & 0x00ff, (ext[2] >> 8) & 0x00ff)
                } else {
                    (TAB_DEFAULT_WIDTH, 0u16, 1u16)
                };
                t_xml.push_str(&format!(
                    r#"<hp:tab width="{}" leader="{}" type="{}"/>"#,
                    width, leader, tab_type
                ));
            }
            '\n' => {
                flush_buf(&mut t_xml, &mut buf);
                t_xml.push_str("<hp:lineBreak/>");
            }
            c if (c as u32) < 0x20 => { /* 기타 제어문자 무시 */ }
            c => buf.push(c),
        }
    }
    flush_buf(&mut t_xml, &mut buf);
    t_xml.push_str("</hp:t>");
    t_xml
}

/// 문단 콘텐츠를 `char_shapes` 경계 기준 다중 `<hp:run>` 으로 분할 출력하는 빌더 (#1378).
///
/// 파서(`src/parser/hwpx/section.rs`)는 각 `<hp:run charPrIDRef>` 시작 위치에서
/// `(utf16_pos, char_shape_id)` 를 기록하고 연속 동일 id 를 dedup 한다.
/// 이 빌더는 그 역방향: `segs[i].0` (i ≥ 1) 위치에서 run 을 닫고 새 run 을 연다.
///
/// 경계 규칙 (구현계획서 1.2):
/// 1. 경계와 슬롯/문자가 같은 위치면 경계 먼저 — 해당 콘텐츠는 새 run 소속 (`cut_before`)
/// 2. 연속 동일 id 경계는 출력 시 skip (IR 은 이미 dedup 상태이나 방어적으로 유지)
/// 3. `char_shapes` 가 비어있으면 단일 run `charPrIDRef="0"`
/// 4. `segs[0].start_pos > 0` 인 비정상 IR 도 첫 run 은 위치 0 부터 시작 (관용 처리)
/// 5. 빈 세그먼트는 `<hp:t></hp:t>` 로 방출 — 재파싱 시 run 시작 entry 위치 보존
struct RunSplitter {
    /// `(start_pos, char_shape_id)` — `[0]` 은 첫 run, `[1..]` 은 cut 경계.
    segs: Vec<(u32, u32)>,
    /// 다음 적용할 경계 인덱스.
    next: usize,
    /// 완성된 run 시퀀스.
    runs: String,
    /// 현재 run 의 내부 콘텐츠 버퍼.
    content: String,
}

impl RunSplitter {
    fn new(para: &Paragraph) -> Self {
        let mut segs: Vec<(u32, u32)> = Vec::new();
        for cs in &para.char_shapes {
            if let Some(&(_, last_id)) = segs.last() {
                if last_id == cs.char_shape_id {
                    continue; // 규칙 2
                }
            }
            segs.push((cs.start_pos, cs.char_shape_id));
        }
        if segs.is_empty() {
            segs.push((0, 0)); // 규칙 3
        }
        Self {
            segs,
            next: 1,
            runs: String::new(),
            content: String::new(),
        }
    }

    /// 경계가 없는 단일 run 문단인지.
    fn single_run(&self) -> bool {
        self.segs.len() == 1
    }

    /// `pos` 위치의 콘텐츠 방출 전에 run 경계 적용이 필요한지 (flush 판단용).
    fn needs_cut(&self, pos: u32) -> bool {
        self.next < self.segs.len() && self.segs[self.next].0 <= pos
    }

    /// `pos` 이하 위치의 경계를 모두 적용 — 규칙 1 (경계 먼저, 콘텐츠는 새 run 소속).
    fn cut_before(&mut self, pos: u32) {
        while self.needs_cut(pos) {
            self.close_run();
            self.next += 1;
        }
    }

    /// 현재 run 을 `<hp:run charPrIDRef>` 로 감싸 완성 목록에 추가.
    fn close_run(&mut self) {
        self.runs.push_str(&format!(
            r#"<hp:run charPrIDRef="{}">"#,
            self.segs[self.next - 1].1
        ));
        if self.content.is_empty() {
            // 규칙 5 — 빈 run 도 <hp:t></hp:t> 로 방출해 재파싱 시 entry 보존
            self.runs.push_str("<hp:t></hp:t>");
        } else {
            self.runs.push_str(&self.content);
            self.content.clear();
        }
        self.runs.push_str("</hp:run>");
    }

    /// 잔여 경계(콘텐츠 끝 이후 시작)를 빈 run 으로 방출하고 마지막 run 을 닫는다.
    fn finish(mut self) -> String {
        while self.next < self.segs.len() {
            self.close_run();
            self.next += 1;
        }
        self.close_run();
        self.runs
    }
}

/// `<hp:ctrl><hp:fieldEnd beginIDRef=".."/></hp:ctrl>` 방출 공통 경로.
fn emit_field_end(out: &mut String, para: &Paragraph, control_idx: usize) {
    if let Some(Control::Field(f)) = para.controls.get(control_idx) {
        if let Ok(xml) = writer_to_string(|w| write_field_end(w, f.field_id)) {
            out.push_str("<hp:ctrl>");
            out.push_str(&xml);
            out.push_str("</hp:ctrl>");
        }
    }
}

/// 문단 텍스트 전체를 char_shapes 경계로 분할하며 `splitter` 에 누적한다.
///
/// `char_offsets` 로 문자 idx → UTF-16 위치를 매핑하므로 IR 내 컨트롤(8 유닛 갭)이
/// 있어도 경계 위치가 어긋나지 않는다.
fn split_text_into(splitter: &mut RunSplitter, para: &Paragraph, tab_idx: &mut usize) {
    let mut text_buf = String::new();
    let mut running_pos = 0u32;
    for (idx, c) in para.text.chars().enumerate() {
        let char_pos = para.char_offsets.get(idx).copied().unwrap_or(running_pos);
        if splitter.needs_cut(char_pos) {
            flush_text_fragment(
                &mut splitter.content,
                &mut text_buf,
                &para.tab_extended,
                tab_idx,
            );
            splitter.cut_before(char_pos);
        }
        text_buf.push(c);
        running_pos = char_pos
            .max(running_pos)
            .saturating_add(char_utf16_width(c));
    }
    flush_text_fragment(
        &mut splitter.content,
        &mut text_buf,
        &para.tab_extended,
        tab_idx,
    );
}

/// Paragraph 본문을 완전한 `<hp:run>` 시퀀스로 직렬화한다 (#1378 다중 run 분할).
fn render_runs(para: &Paragraph, ctx: &mut SerializeContext) -> String {
    // ID 참조 무결성 (구현계획서 1.5): 실제 char_shapes entry 만 reference.
    // 빈 IR 의 fallback 0 은 제외 — char_shapes 미등록 문서(`Document::default()`)의
    // 직렬화를 깨지 않도록.
    for cs in &para.char_shapes {
        ctx.char_shape_ids.reference(cs.char_shape_id);
    }

    let mut splitter = RunSplitter::new(para);

    // Bookmark는 IR에 위치 정보가 없어 문단 시작(첫 run)에 배치한다.
    // (HWPX 파서가 char_count에 포함하지 않아 slot 시스템이 위치를 추적할 수 없음)
    for ctrl in &para.controls {
        if let Control::Bookmark(bm) = ctrl {
            if let Ok(xml) = writer_to_string(|w| write_bookmark(w, bm)) {
                splitter.content.push_str("<hp:ctrl>");
                splitter.content.push_str(&xml);
                splitter.content.push_str("</hp:ctrl>");
            }
        }
    }

    let slot_count = inferred_control_slot_count(para);
    let slots: Vec<&Control> = if slot_count == para.controls.len() {
        para.controls.iter().collect()
    } else {
        // [Task #1379] 셀·글상자 subList(depth > 0) 경로에서는 ColumnDef 도
        // 인라인 슬롯으로 취급한다 (원본 XML 에 <hp:ctrl><hp:colPr/></hp:ctrl> 인라인 존재).
        // 본문(depth 0) 경로는 섹션 템플릿 첫 run 의 colPr 가 받으므로 불변.
        para.controls
            .iter()
            .filter(|c| {
                is_hwpx_fallback_inline_slot(c)
                    || (ctx.sub_list_depth > 0 && matches!(c, Control::ColumnDef(_)))
            })
            .collect()
    };

    let mut tab_idx = 0usize;

    // fast path: 슬롯·필드·경계 없음 — 텍스트 전체를 단일 run 으로
    if slots.is_empty() && para.field_ranges.is_empty() && splitter.single_run() {
        let t = render_hp_t_content(&para.text, &para.tab_extended, &mut tab_idx);
        splitter.content.push_str(&t);
        return splitter.finish();
    }

    // mismatch 경로: 슬롯 위치 추정 불가 — 텍스트(경계 분할 포함) 후 슬롯 일괄 방출
    if slot_count != slots.len() {
        split_text_into(&mut splitter, para, &mut tab_idx);
        for slot in &slots {
            render_control_slot(&mut splitter.content, slot, ctx);
        }
        return splitter.finish();
    }

    // 메인 경로 — UTF-16 위치 축 위에서 슬롯/필드/문자/경계를 함께 처리
    let mut text_buf = String::new();
    let mut slot_idx = 0usize;
    let mut expected_utf16_pos = 0u32;
    let mut field_end_emitted = vec![false; para.field_ranges.len()];

    // 빈 문단(text == "")의 0-length 필드: 메인 루프가 실행되지 않아
    // pre-char 검사를 통과하지 못하므로 루프 전에 slots → fieldEnd 순으로 방출한다.
    if para.text.is_empty() {
        while slot_idx < slots.len() {
            splitter.cut_before(expected_utf16_pos);
            render_control_slot(&mut splitter.content, slots[slot_idx], ctx);
            slot_idx += 1;
            expected_utf16_pos = expected_utf16_pos.saturating_add(8);
        }
        for (i, fr) in para.field_ranges.iter().enumerate() {
            if fr.start_char_idx == fr.end_char_idx && !field_end_emitted[i] {
                splitter.cut_before(expected_utf16_pos);
                emit_field_end(&mut splitter.content, para, fr.control_idx);
                expected_utf16_pos = expected_utf16_pos.saturating_add(8);
                field_end_emitted[i] = true;
            }
        }
    }

    for (idx, c) in para.text.chars().enumerate() {
        let char_pos = para
            .char_offsets
            .get(idx)
            .copied()
            .unwrap_or(expected_utf16_pos);
        while slot_idx < slots.len() && char_pos >= expected_utf16_pos.saturating_add(8) {
            flush_text_fragment(
                &mut splitter.content,
                &mut text_buf,
                &para.tab_extended,
                &mut tab_idx,
            );
            // 슬롯 시작 위치의 경계 — 슬롯은 새 run 소속 (규칙 1)
            splitter.cut_before(expected_utf16_pos);
            render_control_slot(&mut splitter.content, slots[slot_idx], ctx);
            slot_idx += 1;
            expected_utf16_pos = expected_utf16_pos.saturating_add(8);
        }

        // 0-length 필드(start == end == idx): fieldBegin 방출 직후, 문자 push 전에 fieldEnd 방출.
        // post-char 검사(next_idx 기준)는 end-1 번째 문자 처리 후 방출하므로 0-length 필드에서
        // fieldEnd가 fieldBegin 앞에 나오거나 텍스트 뒤로 밀리는 문제가 생긴다.
        for (i, fr) in para.field_ranges.iter().enumerate() {
            if fr.start_char_idx == fr.end_char_idx
                && fr.end_char_idx == idx
                && !field_end_emitted[i]
            {
                flush_text_fragment(
                    &mut splitter.content,
                    &mut text_buf,
                    &para.tab_extended,
                    &mut tab_idx,
                );
                splitter.cut_before(expected_utf16_pos);
                emit_field_end(&mut splitter.content, para, fr.control_idx);
                field_end_emitted[i] = true;
            }
        }

        // AutoNumber 슬롯 (#1382): placeholder 공백이 슬롯 8유닛의 첫 유닛을 점유
        // (HWP5/HWPX 파서 공통 규약 — placeholder at P, 다음 문자 P+8). 슬롯 위치
        // (char_pos == expected)의 placeholder 에서 ctrl 을 방출하고, placeholder 는
        // 파서가 IR 에 합성한 문자이므로 텍스트로 내보내지 않는다 (한컴 원본 XML 동형:
        // `<hp:ctrl><hp:autoNum/></hp:ctrl>` 뒤에 실제 텍스트만 이어진다).
        // placeholder 판별: 일반 공백과 구분하기 위해 직후 offset 의 +8 jump 를 본다
        // (마지막 문자면 jump 가 char_offsets 에 없으므로 placeholder 로 간주).
        let is_autonum_placeholder = c == ' '
            && char_pos == expected_utf16_pos
            && para
                .char_offsets
                .get(idx + 1)
                .copied()
                .unwrap_or_else(|| char_pos.saturating_add(8))
                >= char_pos.saturating_add(8);
        if slot_idx < slots.len()
            && matches!(slots[slot_idx], Control::AutoNumber(_))
            && is_autonum_placeholder
        {
            flush_text_fragment(
                &mut splitter.content,
                &mut text_buf,
                &para.tab_extended,
                &mut tab_idx,
            );
            splitter.cut_before(expected_utf16_pos);
            render_control_slot(&mut splitter.content, slots[slot_idx], ctx);
            slot_idx += 1;
            expected_utf16_pos = expected_utf16_pos.saturating_add(8);
            continue;
        }

        // 문자 위치의 경계 — 문자는 새 run 소속 (규칙 1)
        if splitter.needs_cut(char_pos) {
            flush_text_fragment(
                &mut splitter.content,
                &mut text_buf,
                &para.tab_extended,
                &mut tab_idx,
            );
            splitter.cut_before(char_pos);
        }

        text_buf.push(c);
        let width = char_utf16_width(c);
        if char_pos >= expected_utf16_pos {
            expected_utf16_pos = char_pos.saturating_add(width);
        } else {
            expected_utf16_pos = expected_utf16_pos.saturating_add(width);
        }

        // end_char_idx는 미포함(exclusive): 현재 문자가 필드 범위의 마지막이면 fieldEnd 삽입.
        // 0-length 필드(start == end)는 위의 pre-char 검사에서 처리하므로 제외한다.
        let next_idx = idx + 1;
        for (i, fr) in para.field_ranges.iter().enumerate() {
            if fr.end_char_idx == next_idx
                && !field_end_emitted[i]
                && fr.start_char_idx < fr.end_char_idx
            {
                flush_text_fragment(
                    &mut splitter.content,
                    &mut text_buf,
                    &para.tab_extended,
                    &mut tab_idx,
                );
                splitter.cut_before(expected_utf16_pos);
                emit_field_end(&mut splitter.content, para, fr.control_idx);
                field_end_emitted[i] = true;
            }
        }
    }

    flush_text_fragment(
        &mut splitter.content,
        &mut text_buf,
        &para.tab_extended,
        &mut tab_idx,
    );

    // end_char_idx >= text.len() 인 경우 루프에서 감지되지 않으므로 루프 후에 처리
    for (i, fr) in para.field_ranges.iter().enumerate() {
        if !field_end_emitted[i] {
            splitter.cut_before(expected_utf16_pos);
            emit_field_end(&mut splitter.content, para, fr.control_idx);
            expected_utf16_pos = expected_utf16_pos.saturating_add(8);
        }
    }

    while slot_idx < slots.len() {
        splitter.cut_before(expected_utf16_pos);
        render_control_slot(&mut splitter.content, slots[slot_idx], ctx);
        slot_idx += 1;
        expected_utf16_pos = expected_utf16_pos.saturating_add(8);
    }

    splitter.finish()
}

fn inferred_control_slot_count(para: &Paragraph) -> usize {
    // [#1382] AutoNumber 는 placeholder 공백이 슬롯 8유닛의 첫 유닛을 점유한다
    // (HWP5 body_text / HWPX offsets 조립 공통 규약). placeholder 가 텍스트 폭에
    // 이미 1유닛으로 집계되므로 두 축 모두에서 autoNum 1개당 잉여가 8이 아닌 7로
    // 측정된다 → autoNum 수만큼 더해 보정한 뒤 8로 나눈다. placeholder 없는
    // 합성 IR(편집기 생성)은 잉여 0이라 보정해도 0 — 기존 mismatch 경로 유지.
    let autonum_count = para
        .controls
        .iter()
        .filter(|c| matches!(c, Control::AutoNumber(_)))
        .count() as u32;

    let text_units: u32 = para.text.chars().map(char_utf16_width).sum();
    let from_char_count = para
        .char_count
        .saturating_sub(1)
        .saturating_sub(text_units)
        .saturating_add(autonum_count)
        / 8;

    let mut offsets_gap = 0u32;
    let mut expected = 0u32;
    for (idx, c) in para.text.chars().enumerate() {
        let pos = para.char_offsets.get(idx).copied().unwrap_or(expected);
        if pos > expected {
            offsets_gap += pos - expected;
        }
        expected = pos.max(expected).saturating_add(char_utf16_width(c));
    }
    let from_offsets = offsets_gap.saturating_add(autonum_count) / 8;

    // fieldEnd는 8 code unit 슬롯이지만 para.controls[]에 대응 컨트롤이 없다.
    // field_ranges.len()이 fieldEnd 수와 정확히 일치하므로 빼서 보정한다.
    from_char_count
        .max(from_offsets)
        .saturating_sub(para.field_ranges.len() as u32) as usize
}

pub(crate) fn is_hwpx_inline_slot(control: &Control) -> bool {
    matches!(
        control,
        Control::Table(_)
            | Control::Shape(_)
            | Control::Picture(_)
            | Control::CharOverlap(_)
            | Control::Ruby(_)
            | Control::Equation(_)
            | Control::Field(_)
            | Control::Form(_)
            | Control::Footnote(_)
            | Control::Endnote(_)
            | Control::PageHide(_)
            | Control::PageNumberPos(_)
            | Control::NewNumber(_)
            | Control::ColumnDef(_)
            | Control::Header(_)
            | Control::Footer(_)
            | Control::Bookmark(_)
            | Control::Markpen(_)
            | Control::AutoNumber(_)
    )
}

fn is_hwpx_fallback_inline_slot(control: &Control) -> bool {
    is_hwpx_inline_slot(control) && !matches!(control, Control::ColumnDef(_))
}

fn flush_text_fragment(
    out: &mut String,
    text_buf: &mut String,
    tab_extended: &[[u16; 7]],
    tab_idx: &mut usize,
) {
    if !text_buf.is_empty() {
        out.push_str(&render_hp_t_content(text_buf, tab_extended, tab_idx));
        text_buf.clear();
    }
}

fn render_control_slot(out: &mut String, control: &Control, ctx: &mut SerializeContext) {
    match control {
        Control::Equation(eq) => {
            out.push_str(&render_equation(eq));
        }
        Control::Table(tbl) => match writer_to_string(|w| table::write_table(w, tbl, ctx)) {
            Ok(xml) => out.push_str(&xml),
            Err(e) => eprintln!("[hwpx] Table 직렬화 실패: {e}"),
        },
        Control::Picture(pic) => match writer_to_string(|w| picture::write_picture(w, pic, ctx)) {
            Ok(xml) => out.push_str(&xml),
            Err(e) => eprintln!("[hwpx] Picture 직렬화 실패: {e}"),
        },
        Control::Shape(shape) => {
            out.push_str(&render_shape(shape, ctx));
        }
        Control::Footnote(note) => {
            out.push_str(&render_footnote(note, ctx));
        }
        Control::Endnote(note) => {
            out.push_str(&render_endnote(note, ctx));
        }
        Control::Field(f) => {
            // fieldBegin은 <hp:ctrl>...</hp:ctrl>로 감싸야 함 (Table/Picture와 달리)
            match writer_to_string(|w| write_field_begin(w, f)) {
                Ok(xml) => {
                    out.push_str("<hp:ctrl>");
                    out.push_str(&xml);
                    out.push_str("</hp:ctrl>");
                }
                Err(e) => eprintln!("[hwpx] Field 직렬화 실패: {e}"),
            }
        }
        Control::CharOverlap(co) => out.push_str(&render_char_overlap(co, ctx)),
        Control::Bookmark(bm) => out.push_str(&render_bookmark(bm)),
        Control::Markpen(markpen) => out.push_str(&render_markpen(markpen)),
        Control::PageHide(ph) => out.push_str(&render_page_hiding(ph)),
        Control::PageNumberPos(pn) => out.push_str(&render_page_num(pn)),
        Control::NewNumber(nn) => out.push_str(&render_new_num(nn)),
        Control::ColumnDef(cd) if ctx.sub_list_depth > 0 => out.push_str(&render_col_pr_ctrl(cd)),
        Control::ColumnDef(cd) => out.push_str(&render_column_def(cd)),
        Control::Header(h) => out.push_str(&render_header(h, ctx)),
        Control::Footer(f) => out.push_str(&render_footer(f, ctx)),
        Control::AutoNumber(an) => out.push_str(&render_autonum(an)),
        Control::Form(form) => match writer_to_string(|w| super::form::write_form(w, form)) {
            // 폼은 <hp:run> 직접 자식 (Table/Picture와 동일, <hp:ctrl> 비포장)
            Ok(xml) => out.push_str(&xml),
            Err(e) => eprintln!("[hwpx] Form 직렬화 실패: {e}"),
        },
        _ => {}
    }
}

/// 셀·글상자 subList 인라인 `<hp:ctrl><hp:colPr .../></hp:ctrl>` (#1379 3단계).
/// `parse_col_pr` / `parse_col_line`(parser/hwpx/section.rs)의 역매핑.
fn render_col_pr_ctrl(cd: &ColumnDef) -> String {
    let col_type = match cd.column_type {
        ColumnType::Distribute => "BalancedNewspaper",
        ColumnType::Parallel => "Parallel",
        ColumnType::Normal => "NEWSPAPER",
    };
    let layout = match cd.direction {
        ColumnDirection::RightToLeft => "RIGHT",
        ColumnDirection::LeftToRight => "LEFT",
    };
    let mut out = format!(
        r#"<hp:ctrl><hp:colPr id="" type="{}" layout="{}" colCount="{}" sameSz="{}" sameGap="{}""#,
        col_type, layout, cd.column_count, cd.same_width as u8, cd.spacing,
    );
    if cd.separator_type != 0 {
        out.push('>');
        out.push_str(&format!(
            r#"<hp:colLine type="{}" width="{} mm" color="{}"/>"#,
            col_line_type_str(cd.separator_type),
            line_width_mm(cd.separator_width),
            super::shape::color_to_hex(cd.separator_color),
        ));
        out.push_str("</hp:colPr></hp:ctrl>");
    } else {
        out.push_str("/></hp:ctrl>");
    }
    out
}

/// `parse_hwpx_line_type` 역매핑 (colLine type).
fn col_line_type_str(t: u8) -> &'static str {
    match t {
        2 => "DASH",
        3 => "DOT",
        4 => "DASH_DOT",
        5 => "DASH_DOT_DOT",
        6 => "LONG_DASH",
        7 => "CIRCLE",
        _ => "SOLID",
    }
}

/// HWP 선 굵기 인덱스 → mm 수치 문자열 (`parse_hwpx_line_width` 역매핑).
fn line_width_mm(w: u8) -> &'static str {
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
        _ => "5.0",
    }
}

/// `<hp:compose>` 글자겹침(CharOverlap) — `<hp:run>` 직접 자식 (<hp:ctrl> 비포장).
/// `parse_compose`(parser/hwpx/section.rs)의 역매핑. charPr 목록은 미설정(u32::MAX)
/// 항목 포함 원본 그대로 방출한다 (ID 참조 등록 비대상).
fn render_char_overlap(co: &CharOverlap, ctx: &mut SerializeContext) -> String {
    let circle_type = match co.border_type {
        1 => "SHAPE_CIRCLE",
        2 => "SHAPE_REVERSAL_CIRCLE",
        3 => "SHAPE_RECTANGLE",
        4 => "SHAPE_REVERSAL_RECTANGLE",
        5 => "SHAPE_TRIANGLE",
        6 => "SHAPE_REVERSAL_TIRANGLE",
        _ => "CHAR",
    };
    let compose_type = if co.expansion == 1 {
        "OVERLAP"
    } else {
        "SPREAD"
    };
    let text = co.chars.iter().collect::<String>();
    let mut out = format!(
        r#"<hp:compose circleType="{}" charSz="{}" composeType="{}" charPrCnt="{}" composeText="{}">"#,
        circle_type,
        co.inner_char_size,
        compose_type,
        co.char_shape_ids.len(),
        xml_escape(&text),
    );
    for char_shape_id in &co.char_shape_ids {
        let pr_id_ref = if *char_shape_id == u32::MAX {
            "-1".to_string()
        } else {
            reference_char_shape_if_applicable(ctx, *char_shape_id);
            char_shape_id.to_string()
        };
        out.push_str(&format!(r#"<hp:charPr prIDRef="{}"/>"#, pr_id_ref));
    }
    out.push_str("</hp:compose>");
    out
}

fn render_bookmark(bm: &Bookmark) -> String {
    match writer_to_string(|w| write_bookmark(w, bm)) {
        Ok(xml) => {
            let mut out = String::from("<hp:ctrl>");
            out.push_str(&xml);
            out.push_str("</hp:ctrl>");
            out
        }
        Err(_) => String::new(),
    }
}

fn render_markpen(markpen: &Markpen) -> String {
    if markpen.begin {
        let color = markpen.color.as_deref().unwrap_or("#FFFF00");
        format!(r#"<hp:markpenBegin color="{}"/>"#, xml_escape(color))
    } else {
        "<hp:markpenEnd/>".to_string()
    }
}

fn render_column_def(cd: &ColumnDef) -> String {
    let column_type = match cd.column_type {
        ColumnType::Normal => "NEWSPAPER",
        ColumnType::Distribute => "BalancedNewspaper",
        ColumnType::Parallel => "Parallel",
    };
    let layout = match cd.direction {
        ColumnDirection::LeftToRight => "LEFT",
        ColumnDirection::RightToLeft => "RIGHT",
    };
    let same_sz = u8::from(cd.same_width);
    let line = if cd.separator_type == 0 && cd.separator_width == 0 && cd.separator_color == 0 {
        String::new()
    } else {
        format!(
            r#"<hp:colLine type="{}" width="{}" color="{}"/>"#,
            column_line_type_to_hwpx(cd.separator_type),
            column_line_width_to_hwpx(cd.separator_width),
            color_ref_to_hwpx(cd.separator_color),
        )
    };

    if line.is_empty() {
        format!(
            r#"<hp:ctrl><hp:colPr id="" type="{column_type}" layout="{layout}" colCount="{}" sameSz="{same_sz}" sameGap="{}"/></hp:ctrl>"#,
            cd.column_count, cd.spacing,
        )
    } else {
        format!(
            r#"<hp:ctrl><hp:colPr id="" type="{column_type}" layout="{layout}" colCount="{}" sameSz="{same_sz}" sameGap="{}">{line}</hp:colPr></hp:ctrl>"#,
            cd.column_count, cd.spacing,
        )
    }
}

fn column_line_type_to_hwpx(value: u8) -> &'static str {
    match value {
        0 => "NONE",
        1 => "SOLID",
        2 => "DASH",
        3 => "DOT",
        4 => "DASH_DOT",
        5 => "DASH_DOT_DOT",
        6 => "LONG_DASH",
        7 => "CIRCLE",
        _ => "SOLID",
    }
}

fn column_line_width_to_hwpx(value: u8) -> &'static str {
    match value {
        0 => "0.1 mm",
        1 => "0.12 mm",
        2 => "0.15 mm",
        3 => "0.2 mm",
        4 => "0.25 mm",
        5 => "0.3 mm",
        6 => "0.4 mm",
        7 => "0.5 mm",
        8 => "0.6 mm",
        9 => "0.7 mm",
        10 => "1.0 mm",
        11 => "1.5 mm",
        12 => "2.0 mm",
        13 => "3.0 mm",
        14 => "4.0 mm",
        _ => "5.0 mm",
    }
}

/// 장식 문자(userChar/prefixChar/suffixChar)용 속성값. '\0'(미설정)은 빈 문자열.
fn ctrl_char_attr(c: char) -> String {
    if c == '\0' {
        String::new()
    } else {
        xml_escape(&c.to_string())
    }
}

/// `<hp:ctrl><hp:autoNum num=".." numType=".."><hp:autoNumFormat .../></hp:autoNum></hp:ctrl>`
/// 자동 번호(AutoNumber) 컨트롤. format은 pageNum formatType과 동일한 코드→문자열 매핑.
fn render_autonum(an: &AutoNumber) -> String {
    format!(
        concat!(
            r#"<hp:ctrl><hp:autoNum num="{num}" numType="{nt}">"#,
            r#"<hp:autoNumFormat type="{ty}" userChar="{u}" prefixChar="{p}" "#,
            r#"suffixChar="{s}" supscript="{sup}"/></hp:autoNum></hp:ctrl>"#
        ),
        num = an.number,
        nt = auto_number_type_to_str(an.number_type),
        ty = page_num_format_to_str(an.format),
        u = ctrl_char_attr(an.user_symbol),
        p = ctrl_char_attr(an.prefix_char),
        s = ctrl_char_attr(an.suffix_char),
        sup = an.superscript as u8,
    )
}

/// 머리말/꼬리말 적용 범위 → HWPX `applyPageType`. `parse_apply_page_type`의 역매핑.
fn apply_page_type_to_str(a: HeaderFooterApply) -> &'static str {
    match a {
        HeaderFooterApply::Both => "BOTH",
        HeaderFooterApply::Even => "EVEN",
        HeaderFooterApply::Odd => "ODD",
    }
}

/// `<hp:ctrl><hp:{header|footer} applyPageType=".."><hp:subList ...>문단들</hp:subList>...`
/// 머리말/꼬리말은 중첩 문단(subList)을 가진다 — render_note_sublist와 동일한 문단 직렬화
/// 경로(render_paragraph_parts)를 쓰되, subList 텍스트 영역 속성은 IR 보존값을 사용한다.
fn render_header_footer(
    tag: &str,
    h: HeaderFooterFields<'_>,
    ctx: &mut SerializeContext,
) -> String {
    let vert_align = sublist_vert_align_to_hwpx(h.list_attr);
    let mut out = format!(
        concat!(
            r#"<hp:ctrl><hp:{tag} id="{id}" applyPageType="{apply}">"#,
            r#"<hp:subList id="" textDirection="HORIZONTAL" lineWrap="BREAK" vertAlign="{vert_align}" "#,
            r#"linkListIDRef="0" linkListNextIDRef="0" textWidth="{tw}" textHeight="{th}" "#,
            r#"hasTextRef="{tr}" hasNumRef="{nr}">"#
        ),
        tag = tag,
        id = h.id.unwrap_or(0),
        apply = apply_page_type_to_str(h.apply_to),
        vert_align = vert_align,
        tw = h.text_width,
        th = h.text_height,
        tr = h.text_ref,
        nr = h.num_ref,
    );
    let mut vert_cursor: u32 = 0;
    for p in h.paragraphs.iter() {
        let (runs, linesegs, advance) = render_paragraph_parts(p, vert_cursor, ctx);
        vert_cursor = advance;
        out.push_str(&render_hp_p_open_allocated(p, ctx));
        out.push_str(&runs);
        out.push_str(&linesegs);
        out.push_str("</hp:p>");
    }
    out.push_str(&format!("</hp:subList></hp:{tag}></hp:ctrl>", tag = tag));
    out
}

fn sublist_vert_align_to_hwpx(list_attr: u32) -> &'static str {
    match (list_attr >> 21) & 0b11 {
        1 => "CENTER",
        2 => "BOTTOM",
        _ => "TOP",
    }
}

/// render_header_footer 공통 인자 묶음 (Header/Footer가 동일 필드를 가짐).
struct HeaderFooterFields<'a> {
    id: Option<u32>,
    apply_to: HeaderFooterApply,
    list_attr: u32,
    text_width: u32,
    text_height: u32,
    text_ref: u8,
    num_ref: u8,
    paragraphs: &'a [Paragraph],
}

fn render_header(h: &Header, ctx: &mut SerializeContext) -> String {
    render_header_footer(
        "header",
        HeaderFooterFields {
            id: h.hwpx_id,
            apply_to: h.apply_to,
            list_attr: h.list_attr,
            text_width: h.text_width,
            text_height: h.text_height,
            text_ref: h.text_ref,
            num_ref: h.num_ref,
            paragraphs: &h.paragraphs,
        },
        ctx,
    )
}

fn render_footer(f: &Footer, ctx: &mut SerializeContext) -> String {
    render_header_footer(
        "footer",
        HeaderFooterFields {
            id: f.hwpx_id,
            apply_to: f.apply_to,
            list_attr: f.list_attr,
            text_width: f.text_width,
            text_height: f.text_height,
            text_ref: f.text_ref,
            num_ref: f.num_ref,
            paragraphs: &f.paragraphs,
        },
        ctx,
    )
}

/// `<hp:ctrl><hp:pageHiding .../></hp:ctrl>` — 감추기(PageHide) 컨트롤.
/// `parse_page_hiding_attrs`의 역매핑. bool → "0"/"1" (한컴 정합).
fn render_page_hiding(ph: &PageHide) -> String {
    format!(
        concat!(
            r#"<hp:ctrl><hp:pageHiding hideHeader="{}" hideFooter="{}" "#,
            r#"hideMasterPage="{}" hideBorder="{}" hideFill="{}" hidePageNum="{}"/></hp:ctrl>"#
        ),
        ph.hide_header as u8,
        ph.hide_footer as u8,
        ph.hide_master_page as u8,
        ph.hide_border as u8,
        ph.hide_fill as u8,
        ph.hide_page_num as u8,
    )
}

/// 쪽 번호 위치 코드(표 150) → HWPX `pos` 문자열. `parse_page_num_attrs`의 역매핑.
fn page_num_pos_to_str(pos: u8) -> &'static str {
    match pos {
        0 => "NONE",
        1 => "TOP_LEFT",
        2 => "TOP_CENTER",
        3 => "TOP_RIGHT",
        4 => "BOTTOM_LEFT",
        5 => "BOTTOM_CENTER",
        6 => "BOTTOM_RIGHT",
        7 => "OUTSIDE_TOP",
        8 => "OUTSIDE_BOTTOM",
        9 => "INSIDE_TOP",
        10 => "INSIDE_BOTTOM",
        _ => "BOTTOM_CENTER",
    }
}

/// 번호 형식 코드(표 134) → HWPX `formatType` 문자열. `parse_page_num_attrs`의 역매핑.
fn page_num_format_to_str(fmt: u8) -> &'static str {
    match fmt {
        0 => "DIGIT",
        1 => "CIRCLE_DIGIT",
        2 => "ROMAN_CAPITAL",
        3 => "ROMAN_SMALL",
        4 => "LATIN_CAPITAL",
        5 => "LATIN_SMALL",
        6 => "HANGUL",
        7 => "HANJA",
        _ => "DIGIT",
    }
}

/// `<hp:ctrl><hp:pageNum .../></hp:ctrl>` — 쪽 번호 위치(PageNumberPos) 컨트롤.
fn render_page_num(pn: &PageNumberPos) -> String {
    // dash_char 기본값은 '-' (모델: 항상 '-'); '\0'이면 '-'로 폴백.
    let side = if pn.dash_char == '\0' {
        '-'
    } else {
        pn.dash_char
    };
    format!(
        r#"<hp:ctrl><hp:pageNum pos="{}" formatType="{}" sideChar="{}"/></hp:ctrl>"#,
        page_num_pos_to_str(pn.position),
        page_num_format_to_str(pn.format),
        xml_escape(&side.to_string()),
    )
}

/// 번호 종류 → HWPX `numType` 문자열. `parse_num_type`의 역매핑.
///
/// [#1387] Picture 는 "PICTURE" — 한컴 실물 표기. 종전 "FIGURE" 는 한컴 생산 파일에
/// 비실재(samples/hwpx 전수 0건)하고 한컴에디터가 그림 번호로 인식하지 못해 캡션
/// 번호가 미출력됐다 (한컴 판정 실증). 파서는 FIGURE/PICTURE 양쪽 수용 유지.
fn auto_number_type_to_str(t: AutoNumberType) -> &'static str {
    match t {
        AutoNumberType::Page => "PAGE",
        AutoNumberType::Footnote => "FOOTNOTE",
        AutoNumberType::Endnote => "ENDNOTE",
        AutoNumberType::Picture => "PICTURE",
        AutoNumberType::Table => "TABLE",
        AutoNumberType::Equation => "EQUATION",
    }
}

/// `<hp:ctrl><hp:newNum .../></hp:ctrl>` — 새 번호 지정(NewNumber) 컨트롤.
fn render_new_num(nn: &NewNumber) -> String {
    format!(
        r#"<hp:ctrl><hp:newNum num="{}" numType="{}"/></hp:ctrl>"#,
        nn.number,
        auto_number_type_to_str(nn.number_type),
    )
}

fn writer_to_string<F>(f: F) -> Result<String, SerializeError>
where
    F: FnOnce(&mut Writer<Vec<u8>>) -> Result<(), SerializeError>,
{
    let mut writer = Writer::new(Vec::new());
    f(&mut writer)?;
    let bytes = writer.into_inner();
    String::from_utf8(bytes)
        .map_err(|e| SerializeError::XmlError(format!("invalid UTF-8 from XML writer: {e}")))
}

fn render_shape(shape: &ShapeObject, ctx: &mut SerializeContext) -> String {
    // Rectangle: Writer-based serializer (drawText 포함)
    if let ShapeObject::Rectangle(r) = shape {
        return match writer_to_string(|w| super::shape::write_rect(w, r, ctx)) {
            Ok(xml) => xml,
            Err(e) => {
                eprintln!("[hwpx] Shape::Rectangle 직렬화 실패: {e}");
                String::new()
            }
        };
    }
    // Line: Writer-based serializer
    if let ShapeObject::Line(l) = shape {
        return match writer_to_string(|w| super::shape::write_line(w, l, ctx)) {
            Ok(xml) => xml,
            Err(e) => {
                eprintln!("[hwpx] Shape::Line 직렬화 실패: {e}");
                String::new()
            }
        };
    }
    if let ShapeObject::Group(g) = shape {
        let mut xml = match writer_to_string(|w| super::shape::write_container_open(w, &g.common)) {
            Ok(xml) => xml,
            Err(e) => {
                eprintln!("[hwpx] Shape::Group 직렬화 실패: {e}");
                String::new()
            }
        };
        for child in &g.children {
            xml.push_str(&render_shape(child, ctx));
        }
        // 캡션 (#1403) 은 자식 도형 뒤에 방출 — 한컴 실물(aift.hwpx) 순서
        match writer_to_string(|w| super::shape::write_container_close(w, g.caption.as_ref(), ctx))
        {
            Ok(close) => xml.push_str(&close),
            Err(e) => eprintln!("[hwpx] Shape::Group 닫기 실패: {e}"),
        }
        return xml;
    }
    let (tag, c, caption) = match shape {
        ShapeObject::Rectangle(_) | ShapeObject::Line(_) => unreachable!(),
        ShapeObject::Ellipse(e) => ("ellipse", &e.common, &e.drawing.caption),
        ShapeObject::Arc(a) => ("arc", &a.common, &a.drawing.caption),
        ShapeObject::Polygon(p) => ("polygon", &p.common, &p.drawing.caption),
        ShapeObject::Curve(cv) => ("curve", &cv.common, &cv.drawing.caption),
        ShapeObject::Group(_) => unreachable!(),
        ShapeObject::Picture(pic) => {
            return match writer_to_string(|w| picture::write_picture(w, pic, ctx)) {
                Ok(xml) => xml,
                Err(e) => {
                    eprintln!("[hwpx] Shape::Picture 직렬화 실패: {e}");
                    String::new()
                }
            };
        }
        ShapeObject::Chart(ch) => ("chart", &ch.common, &ch.caption),
        ShapeObject::Ole(o) => ("ole", &o.common, &o.caption),
    };
    render_common_shape_xml(tag, c, caption, ctx)
}

fn render_common_shape_xml(
    tag: &str,
    c: &CommonObjAttr,
    caption: &Option<crate::model::shape::Caption>,
    ctx: &mut SerializeContext,
) -> String {
    let mut out = format!(
        concat!(
            r#"<hp:{tag} id="{id}" zOrder="{zo}" textWrap="{tw}" textFlow="BOTH_SIDES" lock="0">"#,
            r#"<hp:sz width="{w}" height="{h}" widthRelTo="ABSOLUTE" heightRelTo="ABSOLUTE"/>"#,
            r#"<hp:pos treatAsChar="{tac}" vertRelTo="{vr}" vertAlign="{va}" horzRelTo="{hr}" horzAlign="{ha}" vertOffset="{vo}" horzOffset="{ho}"/>"#,
            r#"<hp:outMargin left="{ml}" right="{mr}" top="{mt}" bottom="{mb}"/>"#,
        ),
        tag = tag,
        id = c.instance_id,
        zo = c.z_order,
        tw = text_wrap_to_hwpx(c.text_wrap),
        tac = if c.treat_as_char { "1" } else { "0" },
        w = c.width,
        h = c.height,
        vr = vert_rel_to_hwpx(c.vert_rel_to),
        va = vert_align_to_hwpx(c.vert_align),
        hr = horz_rel_to_hwpx(c.horz_rel_to),
        ha = horz_align_to_hwpx(c.horz_align),
        vo = c.vertical_offset,
        ho = c.horizontal_offset,
        ml = c.margin.left,
        mr = c.margin.right,
        mt = c.margin.top,
        mb = c.margin.bottom,
    );
    // 캡션 (#1403) — HWP5 파서는 모든 도형의 캡션을 적재하므로(parser/control/shape.rs)
    // legacy 경로(ellipse/arc/polygon/curve/chart/ole)도 방출해야 소실되지 않는다.
    if let Some(cap) = caption {
        match writer_to_string(|w| super::table::write_caption(w, cap, ctx)) {
            Ok(xml) => out.push_str(&xml),
            Err(e) => eprintln!("[hwpx] Shape({tag}) 캡션 직렬화 실패: {e}"),
        }
    }
    out.push_str(&format!("</hp:{tag}>"));
    out
}

fn render_note_sublist(
    tag: &str,
    number: u16,
    prefix_char: u16,
    suffix_char: u16,
    instance_id: u32,
    paragraphs: &[Paragraph],
    ctx: &mut SerializeContext,
) -> String {
    let mut attrs = format!(r#" number="{num}""#, num = number);
    if prefix_char != 0 {
        attrs.push_str(&format!(r#" prefixChar="{}""#, prefix_char));
    }
    if suffix_char != 0 {
        attrs.push_str(&format!(r#" suffixChar="{}""#, suffix_char));
    }
    if instance_id != 0 {
        attrs.push_str(&format!(r#" instId="{}""#, instance_id));
    }
    let mut out = format!(
        r#"<hp:ctrl><hp:{tag}{attrs}><hp:subList id="" textDirection="HORIZONTAL" lineWrap="BREAK" vertAlign="TOP" linkListIDRef="0" linkListNextIDRef="0" textWidth="0" textHeight="0" hasTextRef="0" hasNumRef="0">"#,
        tag = tag,
        attrs = attrs,
    );
    let mut vert_cursor: u32 = 0;
    for p in paragraphs.iter() {
        let (runs, linesegs, advance) = render_paragraph_parts(p, vert_cursor, ctx);
        vert_cursor = advance;
        out.push_str(&render_hp_p_open_allocated(p, ctx));
        out.push_str(&runs);
        out.push_str(&linesegs);
        out.push_str("</hp:p>");
    }
    out.push_str(&format!("</hp:subList></hp:{tag}></hp:ctrl>", tag = tag));
    out
}

fn render_footnote(note: &Footnote, ctx: &mut SerializeContext) -> String {
    render_note_sublist(
        "footNote",
        note.number,
        note.before_decoration_letter,
        note.after_decoration_letter,
        note.instance_id,
        &note.paragraphs,
        ctx,
    )
}

fn render_endnote(note: &Endnote, ctx: &mut SerializeContext) -> String {
    render_note_sublist(
        "endNote",
        note.number,
        note.before_decoration_letter,
        note.after_decoration_letter,
        note.instance_id,
        &note.paragraphs,
        ctx,
    )
}

fn render_equation(eq: &Equation) -> String {
    let c = &eq.common;
    let id = c.instance_id.to_string();
    let z_order = c.z_order.to_string();
    let version = xml_escape(&eq.version_info);
    let baseline = eq.baseline.to_string();
    let text_color = color_ref_to_hwpx(eq.color);
    let base_unit = eq.font_size.to_string();
    let font = xml_escape(&eq.font_name);
    let script = xml_escape(&eq.script);
    let width = c.width.to_string();
    let height = c.height.to_string();
    let treat = if c.treat_as_char { "1" } else { "0" };
    let vert_offset = c.vertical_offset.to_string();
    let horz_offset = c.horizontal_offset.to_string();
    let margin_left = c.margin.left.to_string();
    let margin_right = c.margin.right.to_string();
    let margin_top = c.margin.top.to_string();
    let margin_bottom = c.margin.bottom.to_string();

    format!(
        r#"<hp:equation id="{id}" zOrder="{z_order}" numberingType="EQUATION" textWrap="{}" textFlow="BOTH_SIDES" lock="0" dropcapstyle="None" instid="{id}" version="{version}" baseLine="{baseline}" textColor="{text_color}" baseUnit="{base_unit}" font="{font}"><hp:script>{script}</hp:script><hp:sz width="{width}" widthRelTo="ABSOLUTE" height="{height}" heightRelTo="ABSOLUTE"/><hp:pos treatAsChar="{treat}" affectLSpacing="0" flowWithText="1" allowOverlap="0" holdAnchorAndSO="0" vertRelTo="{}" horzRelTo="{}" vertAlign="{}" horzAlign="{}" vertOffset="{vert_offset}" horzOffset="{horz_offset}"/><hp:outMargin left="{margin_left}" right="{margin_right}" top="{margin_top}" bottom="{margin_bottom}"/></hp:equation>"#,
        text_wrap_to_hwpx(c.text_wrap),
        vert_rel_to_hwpx(c.vert_rel_to),
        horz_rel_to_hwpx(c.horz_rel_to),
        vert_align_to_hwpx(c.vert_align),
        horz_align_to_hwpx(c.horz_align),
    )
}

fn char_utf16_width(c: char) -> u32 {
    if c == '\t' {
        8
    } else if (c as u32) > 0xFFFF {
        2
    } else {
        1
    }
}

fn color_ref_to_hwpx(color: u32) -> String {
    if color == 0xFFFFFFFF {
        return "none".to_string();
    }

    let a = (color >> 24) & 0xFF;
    let r = color & 0xFF;
    let g = (color >> 8) & 0xFF;
    let b = (color >> 16) & 0xFF;
    if a == 0 {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{a:02X}{r:02X}{g:02X}{b:02X}")
    }
}

fn text_wrap_to_hwpx(wrap: TextWrap) -> &'static str {
    match wrap {
        TextWrap::Square => "SQUARE",
        TextWrap::Tight => "TIGHT",
        TextWrap::Through => "THROUGH",
        TextWrap::TopAndBottom => "TOP_AND_BOTTOM",
        TextWrap::BehindText => "BEHIND_TEXT",
        TextWrap::InFrontOfText => "IN_FRONT_OF_TEXT",
    }
}

fn vert_rel_to_hwpx(rel: VertRelTo) -> &'static str {
    match rel {
        VertRelTo::Paper => "PAPER",
        VertRelTo::Page => "PAGE",
        VertRelTo::Para => "PARA",
    }
}

fn horz_rel_to_hwpx(rel: HorzRelTo) -> &'static str {
    match rel {
        HorzRelTo::Paper => "PAPER",
        HorzRelTo::Page => "PAGE",
        HorzRelTo::Column => "COLUMN",
        HorzRelTo::Para => "PARA",
    }
}

fn vert_align_to_hwpx(align: VertAlign) -> &'static str {
    match align {
        VertAlign::Top => "TOP",
        VertAlign::Center => "CENTER",
        VertAlign::Bottom => "BOTTOM",
        VertAlign::Inside => "INSIDE",
        VertAlign::Outside => "OUTSIDE",
    }
}

fn horz_align_to_hwpx(align: HorzAlign) -> &'static str {
    match align {
        HorzAlign::Left => "LEFT",
        HorzAlign::Center => "CENTER",
        HorzAlign::Right => "RIGHT",
        HorzAlign::Inside => "INSIDE",
        HorzAlign::Outside => "OUTSIDE",
    }
}

/// IR의 `line_segs` 를 그대로 XML로 직렬화 (9개 필드 전부 IR 값 사용).
///
/// rhwp 는 자신의 문서에서 비표준 lineseg 를 **새로 생산하지 않는다**.
/// 원본 한컴 파일의 lineseg 값이 파서에 의해 `Paragraph.line_segs` 에 담겼다면,
/// 저장 시 그 값을 훼손 없이 보존한다.
fn render_lineseg_array_from_ir(segs: &[LineSeg]) -> String {
    let mut out = String::new();
    for seg in segs {
        out.push_str(&format!(
            r#"<hp:lineseg textpos="{}" vertpos="{}" vertsize="{}" textheight="{}" baseline="{}" spacing="{}" horzpos="{}" horzsize="{}" flags="{}"/>"#,
            seg.text_start,
            seg.vertical_pos,
            seg.line_height,
            seg.text_height,
            seg.baseline_distance,
            seg.line_spacing,
            seg.column_start,
            seg.segment_width,
            seg.tag,
        ));
    }
    out
}

/// IR 기반 다음 문단의 vert_start 계산 — 마지막 lineseg 의 vpos + lh 사용.
fn next_vert_cursor_from_ir(segs: &[LineSeg], vert_start: u32) -> u32 {
    if let Some(last) = segs.last() {
        // vertical_pos 는 섹션 시작 기준 절대값일 수도, 문단 기준 상대값일 수도 있음.
        // 현재 rhwp 는 섹션 절대값이므로 그대로 + lh 로 다음 커서 산출.
        let next = (last.vertical_pos as i64) + (last.line_height.max(0) as i64);
        if next > vert_start as i64 {
            next as u32
        } else {
            vert_start + VERT_STEP
        }
    } else {
        vert_start + VERT_STEP
    }
}

/// Fallback — IR 에 line_segs 가 없는 경우에만 사용 (예: `Document::default()`).
fn render_lineseg_array_fallback(text: &str, vert_start: u32) -> (String, u32) {
    let mut linesegs = String::new();
    push_lineseg_static(&mut linesegs, 0, vert_start);
    let mut utf16_pos: u32 = 0;
    let mut lines_in_para: u32 = 0;
    for c in text.chars() {
        let u16_len = c.len_utf16() as u32;
        match c {
            '\t' | '\n' => {
                utf16_pos += u16_len;
                if c == '\n' {
                    lines_in_para += 1;
                    push_lineseg_static(
                        &mut linesegs,
                        utf16_pos,
                        vert_start + lines_in_para * VERT_STEP,
                    );
                }
            }
            c if (c as u32) < 0x20 => {}
            _ => utf16_pos += u16_len,
        }
    }
    let vert_end = vert_start + (lines_in_para + 1) * VERT_STEP;
    (linesegs, vert_end)
}

fn push_lineseg_static(out: &mut String, textpos: u32, vertpos: u32) {
    out.push_str(&format!(
        r#"<hp:lineseg textpos="{}" vertpos="{}" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="42520" flags="393216"/>"#,
        textpos, vertpos,
    ));
}

fn flush_buf(t_xml: &mut String, buf: &mut String) {
    if !buf.is_empty() {
        t_xml.push_str(&xml_escape(buf));
        buf.clear();
    }
}

/// 템플릿의 첫 `<hp:linesegarray>...</hp:linesegarray>` **요소 전체**를
/// `new_element` 로 치환한다. `new_element` 가 빈 문자열이면 요소가 제거된다
/// (#1380 — line_segs 빈 문단의 linesegarray 방출 생략).
fn replace_first_linesegs(xml: &str, new_element: &str) -> String {
    let open = xml
        .find(LINESEG_SLOT_OPEN)
        .expect("template has linesegarray");
    let close_rel = xml[open..]
        .find(LINESEG_SLOT_CLOSE)
        .expect("template has closing linesegarray");
    let elem_end = open + close_rel + LINESEG_SLOT_CLOSE.len();
    let mut out = String::with_capacity(xml.len() + new_element.len());
    out.push_str(&xml[..open]);
    out.push_str(new_element);
    out.push_str(&xml[elem_end..]);
    out
}

/// [#1166] 템플릿 pagePr 의 고정 용지 속성(landscape/width/height)을 IR page_def
/// 값으로 치환한다. 종전엔 템플릿 하드코딩값(landscape="WIDELY" width=59528
/// height=84186)이 그대로 출력되어 HWPX 저장 시 가로/세로 + 용지 크기가 손실됐다.
///
/// OWPML landscape: WIDELY=세로(landscape=false), NARROWLY=가로(landscape=true).
/// width/height 는 짧은변/긴변 그대로 (HWP 바이너리 동일 규약).
///
/// [#1388] 확장: gutterType(제본 방향) + `<hp:margin>` 여백 7필드를 IR 값으로 치환.
/// 종전엔 템플릿 고정 여백(left/right=8504 등)이 그대로 출력되어 원본 여백이
/// 변형됐다 (samples/hwpx 전수 51/74 섹션 영향, 본문 +56.7px 시프트·페이지 수 변동).
/// gutterType ↔ binding 매핑은 parser/hwpx/section.rs `parse_page_pr` 의 역매핑.
fn replace_page_pr(xml: &str, page_def: &crate::model::page::PageDef) -> String {
    // 템플릿의 pagePr 여는 태그(고정 문자열) → IR 기반으로 교체.
    const TEMPLATE_PAGE_PR: &str =
        r#"<hp:pagePr landscape="WIDELY" width="59528" height="84186" gutterType="LEFT_ONLY">"#;
    const TEMPLATE_PAGE_MARGIN: &str = r#"<hp:margin header="4252" footer="4252" gutter="0" left="8504" right="8504" top="5668" bottom="4252"/>"#;
    let landscape = if page_def.landscape {
        "NARROWLY"
    } else {
        "WIDELY"
    };
    let gutter_type = match page_def.binding {
        crate::model::page::BindingMethod::SingleSided => "LEFT_ONLY",
        crate::model::page::BindingMethod::DuplexSided => "LEFT_RIGHT",
        crate::model::page::BindingMethod::TopFlip => "TOP_BOTTOM",
    };
    let new_page_pr = format!(
        r#"<hp:pagePr landscape="{}" width="{}" height="{}" gutterType="{}">"#,
        landscape, page_def.width, page_def.height, gutter_type,
    );
    let out = if xml.contains(TEMPLATE_PAGE_PR) {
        xml.replacen(TEMPLATE_PAGE_PR, &new_page_pr, 1)
    } else {
        // 템플릿이 변경됐거나 이미 치환된 경우 — 원본 유지(회귀 방지).
        xml.to_string()
    };
    let new_margin = format!(
        r#"<hp:margin header="{}" footer="{}" gutter="{}" left="{}" right="{}" top="{}" bottom="{}"/>"#,
        page_def.margin_header,
        page_def.margin_footer,
        page_def.margin_gutter,
        page_def.margin_left,
        page_def.margin_right,
        page_def.margin_top,
        page_def.margin_bottom,
    );
    if out.contains(TEMPLATE_PAGE_MARGIN) {
        out.replacen(TEMPLATE_PAGE_MARGIN, &new_margin, 1)
    } else {
        // 템플릿이 변경됐거나 이미 치환된 경우 — 원본 유지(회귀 방지).
        out
    }
}

const TEMPLATE_PAGE_BORDER_FILLS: &str = concat!(
    r#"<hp:pageBorderFill type="BOTH" borderFillIDRef="1" textBorder="PAPER" headerInside="0" footerInside="0" fillArea="PAPER"><hp:offset left="1417" right="1417" top="1417" bottom="1417"/></hp:pageBorderFill>"#,
    r#"<hp:pageBorderFill type="EVEN" borderFillIDRef="1" textBorder="PAPER" headerInside="0" footerInside="0" fillArea="PAPER"><hp:offset left="1417" right="1417" top="1417" bottom="1417"/></hp:pageBorderFill>"#,
    r#"<hp:pageBorderFill type="ODD" borderFillIDRef="1" textBorder="PAPER" headerInside="0" footerInside="0" fillArea="PAPER"><hp:offset left="1417" right="1417" top="1417" bottom="1417"/></hp:pageBorderFill>"#,
);

fn replace_page_border_fills(
    xml: &str,
    section_def: &crate::model::document::SectionDef,
) -> String {
    if !should_render_page_border_fills(section_def) || !xml.contains(TEMPLATE_PAGE_BORDER_FILLS) {
        return xml.to_string();
    }

    let mut rendered = String::new();
    rendered.push_str(&render_page_border_fill(
        page_border_fill_apply_type(&section_def.page_border_fill, "BOTH"),
        &section_def.page_border_fill,
    ));
    for (idx, page_border_fill) in section_def.extra_page_border_fills.iter().enumerate() {
        let fallback_apply_type = match idx {
            0 => "EVEN",
            1 => "ODD",
            _ => "BOTH",
        };
        let apply_type = page_border_fill_apply_type(page_border_fill, fallback_apply_type);
        rendered.push_str(&render_page_border_fill(apply_type, page_border_fill));
    }
    xml.replacen(TEMPLATE_PAGE_BORDER_FILLS, &rendered, 1)
}

fn should_render_page_border_fills(section_def: &crate::model::document::SectionDef) -> bool {
    let page_border_fill = &section_def.page_border_fill;
    page_border_fill.border_fill_id != 0
        || page_border_fill.attr != 0
        || page_border_fill.spacing_left != 0
        || page_border_fill.spacing_right != 0
        || page_border_fill.spacing_top != 0
        || page_border_fill.spacing_bottom != 0
        || !section_def.extra_page_border_fills.is_empty()
}

fn render_page_border_fill(apply_type: &str, page_border_fill: &PageBorderFill) -> String {
    format!(
        r#"<hp:pageBorderFill type="{}" borderFillIDRef="{}" textBorder="{}" headerInside="{}" footerInside="{}" fillArea="{}"><hp:offset left="{}" right="{}" top="{}" bottom="{}"/></hp:pageBorderFill>"#,
        apply_type,
        page_border_fill.border_fill_id,
        page_border_fill_text_border(page_border_fill),
        u8::from(page_border_fill.attr & 0x0000_0002 != 0),
        u8::from(page_border_fill.attr & 0x0000_0004 != 0),
        page_border_fill_fill_area(page_border_fill),
        page_border_fill.spacing_left,
        page_border_fill.spacing_right,
        page_border_fill.spacing_top,
        page_border_fill.spacing_bottom,
    )
}

fn page_border_fill_apply_type<'a>(
    page_border_fill: &PageBorderFill,
    fallback: &'a str,
) -> &'a str {
    match page_border_fill.apply_type {
        PageBorderFillApply::Both => "BOTH",
        PageBorderFillApply::Even => "EVEN",
        PageBorderFillApply::Odd => "ODD",
        PageBorderFillApply::Unknown => fallback,
    }
}

fn page_border_fill_text_border(page_border_fill: &PageBorderFill) -> &'static str {
    if page_border_fill.attr & 0x0000_0001 != 0 {
        "PAPER"
    } else {
        "CONTENT"
    }
}

fn page_border_fill_fill_area(page_border_fill: &PageBorderFill) -> &'static str {
    if page_border_fill.attr & 0x0000_0008 != 0 {
        "PAGE"
    } else if page_border_fill.attr & 0x0000_0010 != 0 {
        "BORDER"
    } else {
        "PAPER"
    }
}

// `TEMPLATE_TEXT_RUN` 는 패턴 인식용 상수로만 쓰이므로 명시 참조.
#[allow(dead_code)]
fn _template_anchor_hint() {
    let _ = TEMPLATE_TEXT_RUN;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::paragraph::{CharShapeRef, Paragraph};

    fn make_doc_with_paragraph(para: Paragraph) -> (Document, Section) {
        let mut section = Section::default();
        section.paragraphs.push(para);
        let mut doc = Document::default();
        doc.sections.push(section.clone());
        (doc, section)
    }

    fn xml_after_section_prefix_run(xml: &str) -> &str {
        xml.find("</hp:secPr>")
            .and_then(|sec_pr_end| {
                xml[sec_pr_end..]
                    .find("</hp:run>")
                    .map(|run_end| sec_pr_end + run_end + "</hp:run>".len())
            })
            .map(|body_start| &xml[body_start..])
            .unwrap_or(xml)
    }

    #[test]
    fn first_paragraph_lineseg_replacement_ignores_nested_header_linesegs() {
        let mut nested = Paragraph::default();
        nested.text = "nested".to_string();
        nested.line_segs.push(LineSeg {
            text_start: 0,
            vertical_pos: 0,
            line_height: 2222,
            text_height: 2222,
            baseline_distance: 1111,
            line_spacing: 333,
            column_start: 0,
            segment_width: 12345,
            tag: LineSeg::TAG_SINGLE_SEGMENT_LINE,
        });

        let mut para = Paragraph::default();
        para.controls.push(Control::Header(Box::new(Header {
            paragraphs: vec![nested],
            text_width: 46800,
            text_height: 3660,
            ..Header::default()
        })));
        para.line_segs.push(LineSeg {
            text_start: 0,
            vertical_pos: 0,
            line_height: 1729,
            text_height: 1729,
            baseline_distance: 1288,
            line_spacing: 0,
            column_start: 0,
            segment_width: 46800,
            tag: LineSeg::TAG_SINGLE_SEGMENT_LINE,
        });

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        assert!(
            xml.contains(r#"vertsize="2222" textheight="2222" baseline="1111" spacing="333" horzpos="0" horzsize="12345""#),
            "nested header lineseg should survive as nested metadata: {}",
            xml
        );
        assert!(
            xml.contains(r#"</hp:run><hp:linesegarray><hp:lineseg textpos="0" vertpos="0" vertsize="1729" textheight="1729" baseline="1288" spacing="0" horzpos="0" horzsize="46800" flags="393216"/></hp:linesegarray>"#),
            "top-level first paragraph lineseg should be replaced at the template slot, not inside the nested header: {}",
            xml
        );
    }

    #[test]
    fn hp_p_attrs_reflect_para_shape_id_and_style_id() {
        let mut para = Paragraph::default();
        para.para_shape_id = 7;
        para.style_id = 3;
        para.text = "hi".to_string();
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(
            xml.contains(r#"paraPrIDRef="7""#),
            "<hp:p> must reflect para_shape_id=7: {}",
            &xml[..200.min(xml.len())]
        );
        assert!(
            xml.contains(r#"styleIDRef="3""#),
            "<hp:p> must reflect style_id=3"
        );
    }

    #[test]
    fn hp_p_preserves_id_from_raw_header_extra() {
        let mut para = Paragraph::default();
        para.text = "hi".to_string();
        para.raw_header_extra = vec![0, 0, 0, 0, 0, 0, 0x78, 0x56, 0x34, 0x12];
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        assert!(
            xml.contains(r#"<hp:p id="305419896""#),
            "<hp:p> id should preserve raw_header_extra instance id: {}",
            &xml[..200.min(xml.len())]
        );
    }

    #[test]
    fn hp_run_reflects_first_char_shape_id() {
        let mut para = Paragraph::default();
        para.text = "hello".to_string();
        para.char_shapes.push(CharShapeRef {
            start_pos: 0,
            char_shape_id: 42,
        });
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(
            xml.contains(r#"<hp:run charPrIDRef="42"><hp:t>hello</hp:t>"#),
            "first run must use char_shape_id 42, xml excerpt around <hp:t>: {:?}",
            xml.find("<hp:t>")
                .map(|i| &xml[i.saturating_sub(50)..(i + 50).min(xml.len())])
        );
    }

    // ---------- #1382: autoNum placeholder 슬롯 ----------

    /// autoNum IR 규약 문단: 텍스트 "가·placeholder·나", offsets [0,1,9].
    fn autonum_para() -> Paragraph {
        let mut para = Paragraph::default();
        para.text = "가 나".to_string(); // 가(0) + placeholder 공백(1) + 나(9)
        para.char_offsets = vec![0, 1, 9];
        para.char_count = 11; // offsets 축 총 10 + 끝 마커 1
        para.controls
            .push(crate::model::control::Control::AutoNumber(
                crate::model::control::AutoNumber {
                    number_type: crate::model::control::AutoNumberType::Footnote,
                    ..Default::default()
                },
            ));
        para
    }

    #[test]
    fn task1382_inference_counts_autonum_placeholder_slot() {
        // placeholder 가 8유닛 중 1유닛을 점유해 잉여가 7로 측정되는 패턴 —
        // autoNum 보정으로 슬롯 1개로 집계되어야 한다 (종전 0 → mismatch 경로).
        assert_eq!(inferred_control_slot_count(&autonum_para()), 1);
    }

    #[test]
    fn task1382_autonum_slot_emitted_at_placeholder() {
        // ctrl 이 placeholder 원위치(mid-text)에 방출되고 placeholder 는 텍스트로
        // 내보내지 않는다 (한컴 원본 XML 동형).
        let (doc, section) = make_doc_with_paragraph(autonum_para());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(
            xml.contains("<hp:t>가</hp:t><hp:ctrl><hp:autoNum"),
            "ctrl 은 placeholder 위치(가 직후)에 방출: {}",
            xml
        );
        assert!(
            xml.contains("</hp:ctrl><hp:t>나</hp:t>"),
            "placeholder 공백은 텍스트로 미방출, 후속 텍스트만 이어짐: {}",
            xml
        );
        assert!(
            !xml.contains("<hp:t>가 나</hp:t>"),
            "종전 결함(슬롯 끝 방출 + placeholder 텍스트 이중 방출) 재발 금지"
        );
    }

    #[test]
    fn task1382_synthetic_autonum_without_placeholder_keeps_legacy_path() {
        // 합성 IR(placeholder/offset 없는 편집기 생성 문단)은 보정 후에도 추론 0 —
        // 기존 mismatch 경로(끝 방출) 유지로 회귀 없음.
        let mut para = Paragraph::default();
        para.text = "가나".to_string();
        para.char_offsets = vec![0, 1];
        para.char_count = 3;
        para.controls
            .push(crate::model::control::Control::AutoNumber(
                crate::model::control::AutoNumber::default(),
            ));
        assert_eq!(inferred_control_slot_count(&para), 0);
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(
            xml.contains("<hp:t>가나</hp:t><hp:ctrl><hp:autoNum"),
            "합성 IR 은 텍스트 후 끝 방출(기존 동작): {}",
            xml
        );
    }

    // ---------- #1388: replace_page_pr — secPr 페이지 여백 원본 보존 ----------

    #[test]
    fn task1388_page_margin_reflects_page_def() {
        let mut para = Paragraph::default();
        para.text = "m".to_string();
        let (doc, mut section) = make_doc_with_paragraph(para);
        let pd = &mut section.section_def.page_def;
        pd.width = 59527;
        pd.height = 84189;
        pd.margin_header = 4251;
        pd.margin_footer = 4251;
        pd.margin_gutter = 0;
        pd.margin_left = 7086;
        pd.margin_right = 14173;
        pd.margin_top = 4251;
        pd.margin_bottom = 4251;
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(
            xml.contains(
                r#"<hp:margin header="4251" footer="4251" gutter="0" left="7086" right="14173" top="4251" bottom="4251"/>"#
            ),
            "hp:margin must reflect IR PageDef 7 fields (온새미로 sec0 실측값)"
        );
        assert!(
            !xml.contains(r#"left="8504""#),
            "template margin value must not survive"
        );
        // #1166 동적화 회귀 없음 — width/height 동반 검증.
        assert!(xml.contains(r#"width="59527" height="84189""#));
    }

    #[test]
    fn task1388_gutter_type_reflects_binding() {
        use crate::model::page::BindingMethod;
        for (binding, expected) in [
            (BindingMethod::SingleSided, r#"gutterType="LEFT_ONLY""#),
            (BindingMethod::DuplexSided, r#"gutterType="LEFT_RIGHT""#),
            (BindingMethod::TopFlip, r#"gutterType="TOP_BOTTOM""#),
        ] {
            let mut para = Paragraph::default();
            para.text = "g".to_string();
            let (doc, mut section) = make_doc_with_paragraph(para);
            section.section_def.page_def.binding = binding;
            let mut ctx = SerializeContext::collect_from_document(&doc);
            let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
            let xml = std::str::from_utf8(&bytes).unwrap();
            assert!(
                xml.contains(expected),
                "binding={:?} must emit {}",
                binding,
                expected
            );
        }
    }

    #[test]
    fn task1388_template_mismatch_keeps_original() {
        // 템플릿 anchor 가 없는 입력 — 원본 유지(silent no-op, 회귀 방지 정책).
        let mut pd = crate::model::page::PageDef::default();
        pd.margin_left = 1234;
        let xml = r#"<hp:pagePr landscape="WIDELY" width="1" height="2" gutterType="LEFT_ONLY"><hp:margin header="0" footer="0" gutter="0" left="9" right="9" top="9" bottom="9"/></hp:pagePr>"#;
        assert_eq!(replace_page_pr(xml, &pd), xml);
    }

    #[test]
    fn task1388_template_anchor_present_in_template() {
        // 템플릿 변경 시 silent no-op 으로 빠지지 않도록 anchor 존재를 직접 보장한다.
        let pd = crate::model::page::PageDef {
            margin_header: 1,
            margin_footer: 2,
            margin_gutter: 3,
            margin_left: 4,
            margin_right: 5,
            margin_top: 6,
            margin_bottom: 7,
            ..Default::default()
        };
        let out = replace_page_pr(EMPTY_SECTION_XML, &pd);
        assert!(
            out.contains(
                r#"<hp:margin header="1" footer="2" gutter="3" left="4" right="5" top="6" bottom="7"/>"#
            ),
            "EMPTY_SECTION_XML 의 margin anchor 치환이 실패 — 템플릿이 변경됐는지 확인"
        );
    }

    #[test]
    fn hp_run_preserves_multiple_char_shape_segments() {
        let mut para = Paragraph::default();
        para.text = "abcdef".to_string();
        para.char_shapes.push(CharShapeRef {
            start_pos: 0,
            char_shape_id: 10,
        });
        para.char_shapes.push(CharShapeRef {
            start_pos: 3,
            char_shape_id: 20,
        });
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        assert!(
            xml.contains(r#"<hp:run charPrIDRef="10"><hp:t>abc</hp:t></hp:run>"#),
            "first char shape segment should be emitted as its own run: {}",
            xml
        );
        assert!(
            xml.contains(r#"<hp:run charPrIDRef="20"><hp:t>def</hp:t></hp:run>"#),
            "second char shape segment should be emitted as its own run: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_same_char_shape_boundaries_from_hwpx_spans() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>
  <hp:run charPrIDRef="7"><hp:t>B</hp:t></hp:run>
  <hp:run charPrIDRef="7"/>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "AB");
        assert_eq!(
            para.char_shapes.len(),
            1,
            "HWP-compatible char_shapes should still dedup same charPr runs"
        );
        assert_eq!(
            para.hwpx_run_spans.len(),
            3,
            "HWPX preservation metadata should retain same-charPr run boundaries"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        let body_xml = xml_after_section_prefix_run(xml);

        assert_eq!(
            body_xml.matches(r#"<hp:run charPrIDRef="7">"#).count(),
            3,
            "same charPr run boundaries and empty run should survive: {}",
            xml
        );
        assert!(xml.contains(r#"<hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>"#));
        assert!(xml.contains(r#"<hp:run charPrIDRef="7"><hp:t>B</hp:t></hp:run>"#));
        assert!(xml.contains(r#"<hp:run charPrIDRef="7"></hp:run>"#));
    }

    #[test]
    fn hp_run_preserves_inline_object_span_boundaries() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>
  <hp:run charPrIDRef="8">
    <hp:tbl rowCnt="1" colCnt="1" cellSpacing="0" borderFillIDRef="0">
      <hp:inMargin left="0" right="0" top="0" bottom="0"/>
      <hp:tr>
        <hp:tc name="0" header="0" hasMargin="0" editable="0" dirty="0" borderFillIDRef="0" textDirection="HORIZONTAL" vertAlign="TOP" colAddr="0" rowAddr="0" colSpan="1" rowSpan="1" width="1000" height="1000">
          <hp:cellAddr colAddr="0" rowAddr="0"/>
          <hp:cellSpan colSpan="1" rowSpan="1"/>
          <hp:cellSz width="1000" height="1000"/>
          <hp:cellMargin left="0" right="0" top="0" bottom="0"/>
          <hp:subList><hp:p paraPrIDRef="0" styleIDRef="0"><hp:run charPrIDRef="0"><hp:t>T</hp:t></hp:run></hp:p></hp:subList>
        </hp:tc>
      </hp:tr>
    </hp:tbl>
  </hp:run>
  <hp:run charPrIDRef="9"><hp:t>B</hp:t></hp:run>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "AB");
        assert_eq!(
            para.hwpx_run_spans.len(),
            3,
            "object-bearing run spans should be retained"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let first = xml
            .find(r#"<hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>"#)
            .expect("first text run should survive");
        let table_run = xml
            .find(r#"<hp:run charPrIDRef="8">"#)
            .expect("table run should survive");
        let table = xml[table_run..]
            .find(r#"<hp:tbl "#)
            .map(|idx| table_run + idx)
            .expect("table should remain in the charPrIDRef=8 run");
        let table_run_end = xml[table_run..]
            .find("</hp:run>")
            .map(|idx| table_run + idx)
            .expect("table run should be closed");
        assert!(
            table < table_run_end,
            "table should be serialized before the charPrIDRef=8 run closes: {}",
            xml
        );
        let last = xml
            .find(r#"<hp:run charPrIDRef="9"><hp:t>B</hp:t></hp:run>"#)
            .expect("last text run should survive");
        assert!(
            first < table_run && table_run < last,
            "run order should remain text, table, text: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_column_def_control_span_boundaries() {
        let source = r##"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>
  <hp:run charPrIDRef="8"><hp:ctrl><hp:colPr id="" type="NEWSPAPER" layout="LEFT" colCount="2" sameSz="1" sameGap="850"><hp:colLine type="SOLID" width="0.12 mm" color="#000000"/></hp:colPr></hp:ctrl></hp:run>
  <hp:run charPrIDRef="9"><hp:t>B</hp:t></hp:run>
</hp:p>
</hs:sec>"##;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "AB");
        assert_eq!(
            para.hwpx_run_spans.len(),
            3,
            "column control run span should be retained"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let first = xml
            .find(r#"<hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>"#)
            .expect("first text run should survive");
        let column_run = xml
            .find(r#"<hp:run charPrIDRef="8">"#)
            .expect("column control run should survive");
        let column = xml[column_run..]
            .find(r#"<hp:ctrl><hp:colPr "#)
            .map(|idx| column_run + idx)
            .expect("colPr control should remain in the charPrIDRef=8 run");
        let column_run_end = xml[column_run..]
            .find("</hp:run>")
            .map(|idx| column_run + idx)
            .expect("column control run should be closed");
        assert!(
            column < column_run_end,
            "colPr should be serialized before the charPrIDRef=8 run closes: {}",
            xml
        );
        assert!(xml.contains(r##"<hp:colLine type="SOLID" width="0.12 mm" color="#000000"/>"##));
        let last = xml
            .find(r#"<hp:run charPrIDRef="9"><hp:t>B</hp:t></hp:run>"#)
            .expect("last text run should survive");
        assert!(
            first < column_run && column_run < last,
            "run order should remain text, column control, text: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_field_begin_end_span_boundaries() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A</hp:t><hp:ctrl><hp:fieldBegin id="42" type="CLICKHERE" name="" editable="1"/></hp:ctrl></hp:run>
  <hp:run charPrIDRef="8"><hp:t>B</hp:t></hp:run>
  <hp:run charPrIDRef="7"><hp:ctrl><hp:fieldEnd beginIDRef="42"/></hp:ctrl><hp:t>C</hp:t></hp:run>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "ABC");
        assert_eq!(para.field_ranges.len(), 1, "field range should be retained");
        assert_eq!(
            para.hwpx_run_spans.len(),
            3,
            "field begin/end run spans should be retained"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        let body_xml = xml_after_section_prefix_run(xml);

        assert_eq!(
            body_xml.matches(r#"<hp:run charPrIDRef="7">"#).count(),
            2,
            "field boundary runs should survive: {}",
            xml
        );
        assert_eq!(
            body_xml.matches(r#"<hp:run charPrIDRef="8">"#).count(),
            1,
            "middle field text run should survive: {}",
            xml
        );
        let begin_run = xml
            .find(r#"<hp:run charPrIDRef="7"><hp:t>A</hp:t><hp:ctrl><hp:fieldBegin "#)
            .expect("fieldBegin should stay after A in the first run");
        let middle_run = xml
            .find(r#"<hp:run charPrIDRef="8"><hp:t>B</hp:t></hp:run>"#)
            .expect("middle text run should survive");
        let end_run = xml[middle_run..]
            .find(r#"<hp:run charPrIDRef="7"><hp:ctrl><hp:fieldEnd "#)
            .map(|idx| middle_run + idx)
            .expect("fieldEnd should stay in the final run");
        assert!(
            begin_run < middle_run && middle_run < end_run,
            "run order should remain field begin, text, field end: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_auto_number_span_boundaries() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>
  <hp:run charPrIDRef="8"><hp:ctrl><hp:autoNum num="1" numType="PAGE"><hp:autoNumFormat type="DIGIT" userChar="" prefixChar="" suffixChar="." supscript="0"/></hp:autoNum></hp:ctrl></hp:run>
  <hp:run charPrIDRef="9"><hp:t>B</hp:t></hp:run>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "A B");
        assert_eq!(
            para.char_offsets,
            vec![0, 1, 9],
            "auto-number placeholder should occupy an 8-unit slot"
        );
        assert_eq!(
            para.hwpx_run_spans.len(),
            3,
            "auto-number run span should be retained"
        );
        let slots = hwpx_slots_with_positions(para).expect("auto-number slot mapping");
        assert_eq!(slots.len(), 1, "auto-number should produce one slot");
        assert_eq!(
            slots[0].pos, 1,
            "auto-number slot should start at the placeholder offset"
        );
        assert!(
            matches!(
                slots[0].kind,
                HwpxRunSlotKind::Control(Control::AutoNumber(_))
            ),
            "auto-number slot should map to the auto-number control"
        );
        assert!(
            can_render_hwpx_run_spans(para),
            "auto-number paragraph should use preserved HWPX run spans"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let first = xml
            .find(r#"<hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>"#)
            .unwrap_or_else(|| panic!("first text run should survive: {}", xml));
        let auto = xml
            .find(r#"<hp:run charPrIDRef="8"><hp:ctrl><hp:autoNum "#)
            .unwrap_or_else(|| panic!("auto-number should stay in a control-only run: {}", xml));
        let auto_end = xml[auto..]
            .find("</hp:run>")
            .map(|idx| auto + idx)
            .expect("auto-number run should be closed");
        assert!(
            !xml[auto..auto_end].contains("<hp:t>"),
            "auto-number placeholder must not be emitted as text: {}",
            xml
        );
        let last = xml
            .find(r#"<hp:run charPrIDRef="9"><hp:t>B</hp:t></hp:run>"#)
            .expect("last text run should survive");
        assert!(
            first < auto && auto < last,
            "run order should remain text, auto-number, text: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_bookmark_zero_width_span_boundary() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>
  <hp:run charPrIDRef="8"><hp:ctrl><hp:bookmark name="mark"/></hp:ctrl><hp:t>B</hp:t></hp:run>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "AB");
        assert_eq!(para.char_offsets, vec![0, 1]);
        assert_eq!(para.hwpx_run_spans.len(), 2);
        assert_eq!(para.hwpx_zero_width_control_slots.len(), 1);
        assert_eq!(para.hwpx_zero_width_control_slots[0].control_idx, 0);
        assert_eq!(para.hwpx_zero_width_control_slots[0].pos, 1);
        assert!(
            can_render_hwpx_run_spans(para),
            "bookmark paragraph should use preserved HWPX run spans"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let first = xml
            .find(r#"<hp:run charPrIDRef="7"><hp:t>A</hp:t></hp:run>"#)
            .unwrap_or_else(|| panic!("first text run should survive: {}", xml));
        let bookmark = xml
            .find(r#"<hp:run charPrIDRef="8"><hp:ctrl><hp:bookmark name="mark"/></hp:ctrl><hp:t>B</hp:t></hp:run>"#)
            .unwrap_or_else(|| panic!("bookmark run should preserve control and text: {}", xml));
        assert!(
            first < bookmark,
            "run order should remain text then bookmark run: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_markpen_range_boundaries() {
        let source = r##"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A<hp:markpenBegin color="#FFFF00"/>BC<hp:markpenEnd/>D</hp:t></hp:run>
</hp:p>
</hs:sec>"##;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "ABCD");
        assert_eq!(para.char_offsets, vec![0, 1, 2, 3]);
        assert_eq!(para.controls.len(), 2);
        assert!(matches!(para.controls[0], Control::Markpen(_)));
        assert!(matches!(para.controls[1], Control::Markpen(_)));
        assert_eq!(para.hwpx_run_spans.len(), 1);
        assert_eq!(para.hwpx_zero_width_control_slots.len(), 2);
        assert_eq!(para.hwpx_zero_width_control_slots[0].control_idx, 0);
        assert_eq!(para.hwpx_zero_width_control_slots[0].pos, 1);
        assert_eq!(para.hwpx_zero_width_control_slots[1].control_idx, 1);
        assert_eq!(para.hwpx_zero_width_control_slots[1].pos, 3);
        assert!(
            can_render_hwpx_run_spans(para),
            "markpen paragraph should use preserved HWPX run spans"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let expected = r##"<hp:run charPrIDRef="7"><hp:t>A</hp:t><hp:markpenBegin color="#FFFF00"/><hp:t>BC</hp:t><hp:markpenEnd/><hp:t>D</hp:t></hp:run>"##;
        assert!(
            xml.contains(expected),
            "markpen boundaries should be preserved inside the original run: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_adjacent_markpen_boundaries_at_same_position() {
        let source = r##"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A<hp:markpenBegin color="#FFFF00"/><hp:markpenEnd/>B</hp:t></hp:run>
</hp:p>
</hs:sec>"##;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "AB");
        assert_eq!(para.hwpx_zero_width_control_slots.len(), 2);
        assert_eq!(para.hwpx_zero_width_control_slots[0].pos, 1);
        assert_eq!(para.hwpx_zero_width_control_slots[1].pos, 1);
        assert!(
            can_render_hwpx_run_spans(para),
            "adjacent markpen boundaries should keep the preserved HWPX run span path"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let expected = r##"<hp:run charPrIDRef="7"><hp:t>A</hp:t><hp:markpenBegin color="#FFFF00"/><hp:markpenEnd/><hp:t>B</hp:t></hp:run>"##;
        assert!(
            xml.contains(expected),
            "adjacent markpen boundaries at the same position should both be preserved: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_markpen_end_at_run_end_boundary() {
        let source = r##"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:t>A<hp:markpenBegin color="#FFFF00"/>B<hp:markpenEnd/></hp:t></hp:run>
</hp:p>
</hs:sec>"##;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "AB");
        assert_eq!(para.hwpx_run_spans[0].end_pos, 2);
        assert_eq!(para.hwpx_zero_width_control_slots.len(), 2);
        assert_eq!(para.hwpx_zero_width_control_slots[0].pos, 1);
        assert_eq!(para.hwpx_zero_width_control_slots[1].pos, 2);

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let expected = r##"<hp:run charPrIDRef="7"><hp:t>A</hp:t><hp:markpenBegin color="#FFFF00"/><hp:t>B</hp:t><hp:markpenEnd/></hp:run>"##;
        assert!(
            xml.contains(expected),
            "markpen end at run end should be preserved before the run closes: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_bookmark_after_inline_object_span_boundary() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7">
    <hp:tbl rowCnt="1" colCnt="1" cellSpacing="0" borderFillIDRef="0">
      <hp:inMargin left="0" right="0" top="0" bottom="0"/>
      <hp:tr>
        <hp:tc name="0" header="0" hasMargin="0" editable="0" dirty="0" borderFillIDRef="0" textDirection="HORIZONTAL" vertAlign="TOP" colAddr="0" rowAddr="0" colSpan="1" rowSpan="1" width="1000" height="1000">
          <hp:cellAddr colAddr="0" rowAddr="0"/>
          <hp:cellSpan colSpan="1" rowSpan="1"/>
          <hp:cellSz width="1000" height="1000"/>
          <hp:cellMargin left="0" right="0" top="0" bottom="0"/>
          <hp:subList><hp:p paraPrIDRef="0" styleIDRef="0"><hp:run charPrIDRef="0"><hp:t>T</hp:t></hp:run></hp:p></hp:subList>
        </hp:tc>
      </hp:tr>
    </hp:tbl>
  </hp:run>
  <hp:run charPrIDRef="8"><hp:ctrl><hp:bookmark name="after-object"/></hp:ctrl></hp:run>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.text, "");
        assert!(para.char_offsets.is_empty());
        assert_eq!(para.hwpx_run_spans.len(), 2);
        assert_eq!(para.hwpx_zero_width_control_slots.len(), 1);
        assert_eq!(para.hwpx_zero_width_control_slots[0].control_idx, 1);
        assert_eq!(para.hwpx_zero_width_control_slots[0].pos, 8);
        assert!(
            can_render_hwpx_run_spans(para),
            "bookmark after inline object should use preserved HWPX run spans"
        );

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let table_run = xml
            .find(r#"<hp:run charPrIDRef="7"><hp:tbl "#)
            .unwrap_or_else(|| panic!("table run should survive: {}", xml));
        let table_run_end = xml[table_run..]
            .find("</hp:run>")
            .map(|idx| table_run + idx)
            .expect("table run should be closed");
        assert!(
            xml[table_run..table_run_end].contains(r#"<hp:tbl "#),
            "table should remain inside the first run: {}",
            xml
        );
        assert!(
            !xml[table_run..table_run_end].contains("after-object"),
            "bookmark should not be merged into the table run: {}",
            xml
        );

        let bookmark = xml
            .find(r#"<hp:run charPrIDRef="8"><hp:ctrl><hp:bookmark name="after-object"/></hp:ctrl></hp:run>"#)
            .unwrap_or_else(|| panic!("bookmark-only run should survive: {}", xml));
        assert!(
            table_run < bookmark,
            "run order should remain table then bookmark-only run: {}",
            xml
        );
    }

    #[test]
    fn hp_run_preserves_empty_t_child_after_inline_object() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7">
    <hp:tbl rowCnt="1" colCnt="1" cellSpacing="0" borderFillIDRef="0">
      <hp:inMargin left="0" right="0" top="0" bottom="0"/>
      <hp:tr>
        <hp:tc name="0" header="0" hasMargin="0" editable="0" dirty="0" borderFillIDRef="0" textDirection="HORIZONTAL" vertAlign="TOP" colAddr="0" rowAddr="0" colSpan="1" rowSpan="1" width="1000" height="1000">
          <hp:cellAddr colAddr="0" rowAddr="0"/>
          <hp:cellSpan colSpan="1" rowSpan="1"/>
          <hp:cellSz width="1000" height="1000"/>
          <hp:cellMargin left="0" right="0" top="0" bottom="0"/>
          <hp:subList><hp:p paraPrIDRef="0" styleIDRef="0"><hp:run charPrIDRef="0"><hp:t>T</hp:t></hp:run></hp:p></hp:subList>
        </hp:tc>
      </hp:tr>
    </hp:tbl>
    <hp:t/>
  </hp:run>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.hwpx_run_spans.len(), 1);
        assert_eq!(para.hwpx_run_spans[0].empty_t_count, 1);

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let table_run = xml
            .find(r#"<hp:run charPrIDRef="7">"#)
            .unwrap_or_else(|| panic!("table run should survive: {}", xml));
        assert!(
            xml[table_run..].contains(r#"<hp:tbl "#),
            "table should remain inside the run: {}",
            xml
        );
        assert!(
            xml[table_run..].contains(r#"<hp:t></hp:t></hp:run>"#),
            "empty hp:t child should be preserved inside the run: {}",
            xml
        );
    }

    #[test]
    fn first_paragraph_preserves_runs_after_template_section_prefix() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0">
  <hp:run charPrIDRef="7"><hp:secPr textDirection="HORIZONTAL"><hp:pagePr landscape="WIDELY" width="59528" height="84186"><hp:margin left="0" right="0" top="0" bottom="0" header="0" footer="0" gutter="0"/></hp:pagePr><hp:colPr id="" type="NEWSPAPER" layout="LEFT" colCount="1" sameSz="1" sameGap="0"/></hp:secPr></hp:run>
  <hp:run charPrIDRef="8"><hp:ctrl><hp:pageNum pos="TOP_LEFT" formatType="DIGIT" sideChar="NONE"/></hp:ctrl></hp:run>
  <hp:run charPrIDRef="9"><hp:t>A</hp:t></hp:run>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let para = &section.paragraphs[0];
        assert_eq!(para.hwpx_run_spans.len(), 3);
        assert!(matches!(
            para.controls.first(),
            Some(Control::SectionDef(_))
        ));
        assert!(matches!(para.controls.get(1), Some(Control::ColumnDef(_))));

        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        let section_run = xml
            .find(r#"<hp:run charPrIDRef="7"><hp:secPr "#)
            .unwrap_or_else(|| panic!("section prefix should keep source charPr: {}", xml));
        let page_num = xml
            .find(r#"<hp:run charPrIDRef="8"><hp:ctrl><hp:pageNum "#)
            .unwrap_or_else(|| panic!("page number run should remain after section run: {}", xml));
        let text = xml
            .find(r#"<hp:run charPrIDRef="9"><hp:t>A</hp:t></hp:run>"#)
            .unwrap_or_else(|| panic!("text run should remain after page number run: {}", xml));
        assert!(
            section_run < page_num && page_num < text,
            "first paragraph run order should remain section, page number, text: {}",
            xml
        );
    }

    #[test]
    fn page_break_paragraph_emits_attr() {
        let mut para = Paragraph::default();
        para.text = "p1".to_string();
        para.column_type = crate::model::paragraph::ColumnBreakType::Page;
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(
            xml.contains(r#"pageBreak="1""#),
            "pageBreak must be 1 for Page column_type"
        );
        assert!(xml.contains(r#"columnBreak="0""#));
    }

    #[test]
    fn default_paragraph_keeps_zero_attrs() {
        let mut para = Paragraph::default();
        para.text = "x".to_string();
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(xml.contains(r#"paraPrIDRef="0""#));
        assert!(xml.contains(r#"styleIDRef="0""#));
        // char_shapes 가 비어있으면 fallback 0
        assert!(xml.contains(r#"<hp:run charPrIDRef="0">"#));
    }

    #[test]
    fn page_pr_roundtrip_preserves_page_def_margins() {
        let mut para = Paragraph::default();
        para.text = "x".to_string();
        let (doc, mut section) = make_doc_with_paragraph(para);
        section.section_def.page_def.margin_left = 5668;
        section.section_def.page_def.margin_right = 5668;
        section.section_def.page_def.margin_top = 2836;
        section.section_def.page_def.margin_bottom = 2836;
        section.section_def.page_def.margin_header = 2836;
        section.section_def.page_def.margin_footer = 2836;
        section.section_def.page_def.margin_gutter = 0;

        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        let reparsed = crate::parser::hwpx::section::parse_hwpx_section(xml).unwrap();
        let page_def = reparsed.section_def.page_def;

        assert_eq!(page_def.margin_left, 5668, "margin_left");
        assert_eq!(page_def.margin_right, 5668, "margin_right");
        assert_eq!(page_def.margin_top, 2836, "margin_top");
        assert_eq!(page_def.margin_bottom, 2836, "margin_bottom");
        assert_eq!(page_def.margin_header, 2836, "margin_header");
        assert_eq!(page_def.margin_footer, 2836, "margin_footer");
        assert_eq!(page_def.margin_gutter, 0, "margin_gutter");
    }

    #[test]
    fn section_roundtrip_preserves_note_pr_shapes() {
        let mut para = Paragraph::default();
        para.text = "x".to_string();
        let (doc, mut section) = make_doc_with_paragraph(para);
        section.section_def.footnote_shape.suffix_char = '\0';
        section.section_def.footnote_shape.separator_length = -1;
        section.section_def.footnote_shape.separator_line_type = 1;
        section.section_def.footnote_shape.separator_line_width = 4;
        section.section_def.footnote_shape.separator_color = 0;
        section.section_def.footnote_shape.separator_margin_bottom = 1000;
        section.section_def.footnote_shape.note_spacing = 0;
        section.section_def.footnote_shape.raw_unknown = 0;
        section.section_def.footnote_shape.numbering = FootnoteNumbering::Continue;
        section.section_def.footnote_shape.placement = FootnotePlacement::EachColumn;

        section.section_def.endnote_shape.suffix_char = '\0';
        section.section_def.endnote_shape.separator_length = -1;
        section.section_def.endnote_shape.separator_line_type = 1;
        section.section_def.endnote_shape.separator_line_width = 4;
        section.section_def.endnote_shape.separator_color = 0;
        section.section_def.endnote_shape.separator_margin_bottom = 1000;
        section.section_def.endnote_shape.note_spacing = 0;
        section.section_def.endnote_shape.raw_unknown = 0;
        section.section_def.endnote_shape.numbering = FootnoteNumbering::Continue;
        section.section_def.endnote_shape.placement = FootnotePlacement::EachColumn;

        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        let reparsed = crate::parser::hwpx::section::parse_hwpx_section(xml).unwrap();

        assert_eq!(reparsed.section_def.footnote_shape.suffix_char, '\0');
        assert_eq!(
            reparsed.section_def.footnote_shape.separator_margin_bottom,
            1000
        );
        assert_eq!(reparsed.section_def.footnote_shape.note_spacing, 0);
        assert_eq!(reparsed.section_def.footnote_shape.separator_line_width, 4);
        assert_eq!(reparsed.section_def.endnote_shape.suffix_char, '\0');
        assert_eq!(reparsed.section_def.endnote_shape.separator_length, -1);
        assert_eq!(reparsed.section_def.endnote_shape.separator_margin_top, -1);
        assert_eq!(
            reparsed.section_def.endnote_shape.separator_margin_bottom,
            1000
        );
        assert_eq!(reparsed.section_def.endnote_shape.note_spacing, 0);
        assert_eq!(reparsed.section_def.endnote_shape.separator_line_width, 4);
    }

    #[test]
    fn page_pr_roundtrip_preserves_page_border_fills() {
        let mut para = Paragraph::default();
        para.text = "x".to_string();
        let (doc, mut section) = make_doc_with_paragraph(para);
        section.section_def.page_border_fill.border_fill_id = 168;
        section.section_def.page_border_fill.attr = 0x0000_0001;
        section.section_def.page_border_fill.spacing_left = 101;
        section.section_def.page_border_fill.spacing_right = 102;
        section.section_def.page_border_fill.spacing_top = 103;
        section.section_def.page_border_fill.spacing_bottom = 104;

        let mut even = section.section_def.page_border_fill.clone();
        even.border_fill_id = 169;
        even.spacing_left = 201;
        let mut odd = section.section_def.page_border_fill.clone();
        odd.border_fill_id = 170;
        odd.spacing_left = 301;
        section.section_def.extra_page_border_fills.push(even);
        section.section_def.extra_page_border_fills.push(odd);

        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        let reparsed = crate::parser::hwpx::section::parse_hwpx_section(xml).unwrap();

        assert_eq!(reparsed.section_def.page_border_fill.border_fill_id, 168);
        assert_eq!(reparsed.section_def.page_border_fill.spacing_left, 101);
        assert_eq!(reparsed.section_def.page_border_fill.spacing_right, 102);
        assert_eq!(reparsed.section_def.page_border_fill.spacing_top, 103);
        assert_eq!(reparsed.section_def.page_border_fill.spacing_bottom, 104);
        assert_eq!(reparsed.section_def.extra_page_border_fills.len(), 2);
        assert_eq!(
            reparsed.section_def.extra_page_border_fills[0].border_fill_id,
            169
        );
        assert_eq!(
            reparsed.section_def.extra_page_border_fills[0].spacing_left,
            201
        );
        assert_eq!(
            reparsed.section_def.extra_page_border_fills[1].border_fill_id,
            170
        );
        assert_eq!(
            reparsed.section_def.extra_page_border_fills[1].spacing_left,
            301
        );
    }

    #[test]
    fn page_pr_roundtrip_preserves_page_border_fill_type_order() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0">
  <hp:run charPrIDRef="0">
    <hp:secPr id="" textDirection="HORIZONTAL" spaceColumns="1134" tabStop="8000" outlineShapeIDRef="1" memoShapeIDRef="0" textVerticalWidthHead="0" masterPageCnt="0">
      <hp:pagePr landscape="WIDELY" width="59528" height="84186" gutterType="LEFT_ONLY">
        <hp:margin header="4252" footer="4252" gutter="0" left="8504" right="8504" top="5668" bottom="4252"/>
      </hp:pagePr>
      <hp:pageBorderFill type="ODD" borderFillIDRef="171" textBorder="PAPER" headerInside="0" footerInside="0" fillArea="PAPER"><hp:offset left="301" right="302" top="303" bottom="304"/></hp:pageBorderFill>
      <hp:pageBorderFill type="EVEN" borderFillIDRef="172" textBorder="CONTENT" headerInside="1" footerInside="0" fillArea="PAGE"><hp:offset left="201" right="202" top="203" bottom="204"/></hp:pageBorderFill>
      <hp:pageBorderFill type="BOTH" borderFillIDRef="173" textBorder="PAPER" headerInside="0" footerInside="1" fillArea="BORDER"><hp:offset left="101" right="102" top="103" bottom="104"/></hp:pageBorderFill>
    </hp:secPr>
  </hp:run>
  <hp:linesegarray><hp:lineseg textpos="0" vertpos="0" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="42520" flags="393216"/></hp:linesegarray>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();

        assert_eq!(
            page_border_fill_type_sequence(xml),
            vec!["ODD", "EVEN", "BOTH"],
            "pageBorderFill type/apply order should survive roundtrip: {}",
            xml
        );
    }

    #[test]
    fn section_roundtrip_preserves_start_num() {
        let source = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0">
  <hp:run charPrIDRef="0">
    <hp:secPr id="" textDirection="HORIZONTAL" spaceColumns="1134" tabStop="8000" outlineShapeIDRef="1" memoShapeIDRef="0" textVerticalWidthHead="0" masterPageCnt="0">
      <hp:pagePr landscape="NARROWLY" width="59528" height="84186" gutterType="LEFT_ONLY">
        <hp:margin header="4252" footer="4252" gutter="0" left="8504" right="8504" top="5668" bottom="4252"/>
      </hp:pagePr>
      <hp:startNum page="7" pic="8" tbl="9" equation="10"/>
    </hp:secPr>
  </hp:run>
  <hp:linesegarray><hp:lineseg textpos="0" vertpos="0" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="42520" flags="393216"/></hp:linesegarray>
</hp:p>
</hs:sec>"#;

        let section = crate::parser::hwpx::section::parse_hwpx_section(source).unwrap();
        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let bytes = write_section(&section, &doc, 0, &mut ctx).unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        let reparsed = crate::parser::hwpx::section::parse_hwpx_section(xml).unwrap();

        assert_eq!(reparsed.section_def.page_num, 7, "page start number");
        assert_eq!(reparsed.section_def.picture_num, 8, "picture start number");
        assert_eq!(reparsed.section_def.table_num, 9, "table start number");
        assert_eq!(
            reparsed.section_def.equation_num, 10,
            "equation start number"
        );
    }

    fn page_border_fill_type_sequence(xml: &str) -> Vec<&str> {
        let mut types = Vec::new();
        for segment in xml.split("<hp:pageBorderFill ").skip(1) {
            if let Some(rest) = segment.strip_prefix(r#"type=""#) {
                if let Some((value, _)) = rest.split_once('"') {
                    types.push(value);
                }
            }
        }
        types
    }

    #[test]
    fn footer_roundtrip_preserves_hwpx_id() {
        let xml = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0">
  <hp:run charPrIDRef="0">
    <hp:ctrl>
      <hp:footer id="3" applyPageType="BOTH">
        <hp:subList id="" textDirection="HORIZONTAL" lineWrap="BREAK" vertAlign="TOP" linkListIDRef="0" linkListNextIDRef="0" textWidth="42520" textHeight="1000" hasTextRef="1" hasNumRef="0">
          <hp:p id="1" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0"><hp:run charPrIDRef="0"><hp:t>foot</hp:t></hp:run><hp:linesegarray><hp:lineseg textpos="0" vertpos="0" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="42520" flags="393216"/></hp:linesegarray></hp:p>
        </hp:subList>
      </hp:footer>
    </hp:ctrl>
  </hp:run>
  <hp:linesegarray><hp:lineseg textpos="0" vertpos="0" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="42520" flags="393216"/></hp:linesegarray>
</hp:p>
</hs:sec>"#;
        let section = crate::parser::hwpx::section::parse_hwpx_section(xml).unwrap();
        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let out = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            out.contains(r#"<hp:footer id="3" applyPageType="BOTH">"#),
            "footer id must be preserved after section parse/write roundtrip"
        );
    }

    #[test]
    fn footer_roundtrip_preserves_sublist_vertical_align() {
        let xml = r#"<hs:sec xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section">
<hp:p id="0" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0">
  <hp:run charPrIDRef="0">
    <hp:ctrl>
      <hp:footer id="3" applyPageType="BOTH">
        <hp:subList id="" textDirection="HORIZONTAL" lineWrap="BREAK" vertAlign="BOTTOM" linkListIDRef="0" linkListNextIDRef="0" textWidth="42520" textHeight="1000" hasTextRef="1" hasNumRef="0">
          <hp:p id="1" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0"><hp:run charPrIDRef="0"><hp:t>foot</hp:t></hp:run><hp:linesegarray><hp:lineseg textpos="0" vertpos="0" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="42520" flags="393216"/></hp:linesegarray></hp:p>
        </hp:subList>
      </hp:footer>
    </hp:ctrl>
  </hp:run>
  <hp:linesegarray><hp:lineseg textpos="0" vertpos="0" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="42520" flags="393216"/></hp:linesegarray>
</hp:p>
</hs:sec>"#;
        let section = crate::parser::hwpx::section::parse_hwpx_section(xml).unwrap();
        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let out = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        let reparsed = crate::parser::hwpx::section::parse_hwpx_section(&out).unwrap();

        let footer = reparsed.paragraphs[0]
            .controls
            .iter()
            .find_map(|control| match control {
                Control::Footer(footer) => Some(footer),
                _ => None,
            })
            .expect("expected footer control");

        assert!(
            out.contains(r#"vertAlign="BOTTOM""#),
            "footer subList vertical align must be serialized from list_attr: {out}"
        );
        assert_eq!((footer.list_attr >> 21) & 0b11, 2);
    }

    #[test]
    fn additional_paragraphs_use_their_own_char_shape() {
        let mut p1 = Paragraph::default();
        p1.text = "first".to_string();
        p1.char_shapes.push(CharShapeRef {
            start_pos: 0,
            char_shape_id: 5,
        });
        let mut p2 = Paragraph::default();
        p2.text = "second".to_string();
        p2.para_shape_id = 2;
        p2.char_shapes.push(CharShapeRef {
            start_pos: 0,
            char_shape_id: 6,
        });
        let mut section = Section::default();
        section.paragraphs.push(p1);
        section.paragraphs.push(p2);
        let mut doc = Document::default();
        doc.sections.push(section.clone());
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        // 두 번째 문단: paraPrIDRef=2, charPrIDRef=6
        assert!(xml.contains(r#"paraPrIDRef="2""#));
        assert!(
            xml.matches(r#"charPrIDRef="6""#).count() >= 1,
            "second paragraph must emit charPrIDRef=6"
        );
    }

    // ---------- #177 Stage 2: IR 기반 lineseg 출력 ----------

    use crate::model::paragraph::LineSeg;

    #[test]
    fn task177_lineseg_reflects_ir_values() {
        // IR에 담긴 lineseg 값이 XML 속성에 그대로 반영되는지 확인.
        let mut para = Paragraph::default();
        para.text = "hello".to_string();
        para.line_segs.push(LineSeg {
            text_start: 0,
            vertical_pos: 5000,
            line_height: 1200,
            text_height: 1100,
            baseline_distance: 900,
            line_spacing: 700,
            column_start: 100,
            segment_width: 50000,
            tag: 999,
        });
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        assert!(xml.contains(r#"<hp:lineseg textpos="0" vertpos="5000" vertsize="1200" textheight="1100" baseline="900" spacing="700" horzpos="100" horzsize="50000" flags="999"/>"#),
            "lineseg must reflect IR values exactly, got XML: {}",
            &xml[xml.find("<hp:lineseg").unwrap_or(0)..(xml.find("<hp:lineseg").unwrap_or(0) + 200).min(xml.len())]);
    }

    #[test]
    fn task177_multiple_linesegs_preserved_in_order() {
        let mut para = Paragraph::default();
        para.text = "three\nlines\nhere".to_string();
        for (i, (tp, vp, lh)) in [(0u32, 0i32, 1000), (6, 1500, 1200), (12, 3100, 1100)]
            .iter()
            .enumerate()
        {
            let _ = i;
            para.line_segs.push(LineSeg {
                text_start: *tp,
                vertical_pos: *vp,
                line_height: *lh,
                text_height: *lh,
                baseline_distance: 850,
                line_spacing: 600,
                column_start: 0,
                segment_width: 42520,
                tag: LineSeg::TAG_SINGLE_SEGMENT_LINE,
            });
        }
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        // 3개 lineseg 모두 출력되고 각각의 vertsize 값이 IR 값과 일치
        assert_eq!(xml.matches("<hp:lineseg ").count(), 3);
        assert!(xml.contains(r#"textpos="0" vertpos="0" vertsize="1000""#));
        assert!(xml.contains(r#"textpos="6" vertpos="1500" vertsize="1200""#));
        assert!(xml.contains(r#"textpos="12" vertpos="3100" vertsize="1100""#));
    }

    #[test]
    fn task1380_linesegarray_omitted_when_ir_empty() {
        // IR 의 line_segs 가 비어있으면 linesegarray 요소 자체를 방출 생략 (#1380).
        // 종전 fallback(vertsize=1000 합성)은 원본 무 → RT 유 비대칭을 만들었다.
        let mut para = Paragraph::default();
        para.text = "a\nb".to_string(); // 텍스트가 있어도 IR 에 lineseg 없으면 생략
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        assert!(
            !xml.contains("<hp:linesegarray"),
            "empty line_segs must omit linesegarray entirely: {}",
            xml
        );
        assert!(!xml.contains("<hp:lineseg "));
    }

    #[test]
    fn task1380_empty_section_omits_linesegarray() {
        // 문단이 없는 섹션(비파싱 IR)도 템플릿의 linesegarray 가 제거되어야 함 (#1380).
        let doc = crate::model::document::Document::default();
        let section = crate::model::document::Section::default();
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        assert!(!xml.contains("<hp:linesegarray"));
    }

    #[test]
    fn task177_ir_lineseg_takes_precedence_over_text() {
        // text 의 \n 개수가 2개(lineseg 3개 기대)이지만 IR의 line_segs 는 1개만 있음.
        // IR 기반 출력이 우선 — 1개만 출력돼야 함.
        let mut para = Paragraph::default();
        para.text = "a\nb\nc".to_string(); // 3줄
        para.line_segs.push(LineSeg {
            text_start: 0,
            vertical_pos: 0,
            line_height: 2000, // IR 값
            text_height: 2000,
            baseline_distance: 1700,
            line_spacing: 300,
            column_start: 0,
            segment_width: 40000,
            tag: 0,
        });
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        // IR 에 1개만 있으므로 lineseg 도 1개만 출력 (rhwp 는 원본 보존)
        assert_eq!(xml.matches("<hp:lineseg ").count(), 1);
        assert!(
            xml.contains(r#"vertsize="2000""#),
            "IR value 2000 must be used, not fallback 1000"
        );
    }

    // ---------- #1289: Bookmark / Field dispatcher 연결 ----------

    use crate::model::control::{Bookmark, CharOverlap, Control, Field, FieldType};
    use crate::model::paragraph::FieldRange;

    #[test]
    fn task1289_bookmark_emits_ctrl_wrapper() {
        // Bookmark는 슬롯 시스템이 위치를 추적할 수 없으므로 문단 시작에 배치한다.
        let mut para = Paragraph::default();
        para.text = "hello".to_string();
        para.char_count = 6; // "hello"(5) + para_end(1)
        para.controls.push(Control::Bookmark(Bookmark {
            name: "test_bm".to_string(),
        }));
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        assert!(
            xml.contains(r#"<hp:ctrl><hp:bookmark name="test_bm"/></hp:ctrl>"#),
            "bookmark must be wrapped in <hp:ctrl>: {}",
            &xml[..300.min(xml.len())]
        );
        assert!(xml.contains("hello"), "text must still be present");
    }

    #[test]
    fn task1289_field_begin_end_roundtrip() {
        // HWPX 파서가 생성하는 구조 시뮬레이션:
        // fieldBegin(8 cu) + "hello"(5 cu) + fieldEnd(8 cu) + para_end(1 cu) = 22
        // para.text 에는 "hello"만 있고 char_offsets 가 +8 오프셋으로 시작한다.
        let mut f = Field::default();
        f.field_type = FieldType::ClickHere;
        f.field_id = 99;

        let mut para = Paragraph::default();
        para.text = "hello".to_string();
        para.char_count = 22;
        para.char_offsets = vec![8, 9, 10, 11, 12];
        para.controls.push(Control::Field(f));
        para.field_ranges.push(FieldRange {
            start_char_idx: 0,
            end_char_idx: 5,
            control_idx: 0,
        });

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            xml.contains(r#"<hp:ctrl><hp:fieldBegin id="99" type="CLICKHERE""#),
            "fieldBegin must be emitted: {}",
            &xml[..500.min(xml.len())]
        );
        assert!(
            xml.contains(r#"<hp:ctrl><hp:fieldEnd beginIDRef="99"/></hp:ctrl>"#),
            "fieldEnd must be emitted: {}",
            &xml[..500.min(xml.len())]
        );
        assert!(xml.contains("hello"), "field text must be present");

        // 순서 검증: fieldBegin < "hello" < fieldEnd
        let begin_pos = xml.find("fieldBegin").expect("fieldBegin");
        let hello_pos = xml.find("hello").expect("hello");
        let end_pos = xml.find("fieldEnd").expect("fieldEnd");
        assert!(begin_pos < hello_pos, "fieldBegin must precede text");
        assert!(hello_pos < end_pos, "text must precede fieldEnd");
    }

    #[test]
    fn task1289_field_end_at_para_boundary() {
        // end_char_idx == text.len() 인 경우: 루프 내 감지 불가 → 루프 후 처리
        let mut f = Field::default();
        f.field_type = FieldType::Date;
        f.field_id = 7;

        let mut para = Paragraph::default();
        para.text = "abc".to_string();
        para.char_count = 20; // fieldBegin(8) + "abc"(3) + fieldEnd(8) + para_end(1)
        para.char_offsets = vec![8, 9, 10];
        para.controls.push(Control::Field(f));
        para.field_ranges.push(FieldRange {
            start_char_idx: 0,
            end_char_idx: 3, // == text.len() → 루프 후 처리 경로
            control_idx: 0,
        });

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            xml.contains(r#"<hp:fieldEnd beginIDRef="7"/>"#),
            "fieldEnd must be emitted even when end_char_idx == text.len(): {}",
            &xml[..400.min(xml.len())]
        );
    }

    #[test]
    fn hwpx_run_slot_emits_char_overlap_compose() {
        let mut para = Paragraph {
            text: "AB".to_string(),
            char_count: 11,
            char_offsets: vec![0, 9],
            ..Default::default()
        };
        para.controls.push(Control::CharOverlap(CharOverlap {
            chars: vec!['가', '나'],
            border_type: 1,
            inner_char_size: 80,
            expansion: 1,
            char_shape_ids: vec![3, 4],
        }));

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            xml.contains("<hp:compose "),
            "compose must be emitted: {}",
            xml
        );
        assert!(
            xml.contains(r#"circleType="SHAPE_CIRCLE""#),
            "compose border type must be preserved: {}",
            xml
        );
        assert!(
            xml.contains(r#"composeText="가나""#),
            "compose text must be emitted: {}",
            xml
        );
        assert_eq!(xml.matches("<hp:charPr ").count(), 2, "{}", xml);

        let a_pos = xml.find('A').expect("A");
        let compose_pos = xml.find("<hp:compose").expect("compose");
        let b_pos = xml.rfind('B').expect("B");
        assert!(
            a_pos < compose_pos,
            "text before compose must remain before it"
        );
        assert!(
            compose_pos < b_pos,
            "text after compose must remain after it"
        );
    }

    #[test]
    fn hwpx_char_overlap_preserves_negative_char_pr_ref_without_context_reference() {
        let mut para = Paragraph {
            text: "A".to_string(),
            char_count: 10,
            char_offsets: vec![8],
            ..Default::default()
        };
        para.controls.push(Control::CharOverlap(CharOverlap {
            chars: vec!['각'],
            char_shape_ids: vec![u32::MAX],
            ..Default::default()
        }));

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            xml.contains(r#"<hp:charPr prIDRef="-1"/>"#),
            "negative HWPX charPr ref must be preserved as -1: {}",
            xml
        );
        assert!(
            ctx.assert_all_refs_resolved().is_ok(),
            "-1 must not be treated as a doc_info char shape reference"
        );
    }

    // ---------- #1298: 0-length field range fieldBegin/fieldEnd 인터리빙 ----------

    #[test]
    fn task1298_zero_length_field_at_para_start() {
        // 0-length 필드 at position 0 (start=0, end=0):
        // HWP stream: fieldBegin(8cu) fieldEnd(8cu) "hello"(5cu) para_end(1cu) = 22cu
        // char_offsets: [16, 17, 18, 19, 20] (fieldBegin+fieldEnd 갭 16 이후 텍스트)
        let mut f = Field::default();
        f.field_type = FieldType::ClickHere;
        f.field_id = 55;

        let mut para = Paragraph::default();
        para.text = "hello".to_string();
        para.char_count = 22;
        para.char_offsets = vec![16, 17, 18, 19, 20];
        para.controls.push(Control::Field(f));
        para.field_ranges.push(FieldRange {
            start_char_idx: 0,
            end_char_idx: 0, // 0-length
            control_idx: 0,
        });

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            xml.contains(r#"<hp:ctrl><hp:fieldBegin id="55""#),
            "fieldBegin must be emitted: {}",
            &xml[..500.min(xml.len())]
        );
        assert!(
            xml.contains(r#"<hp:ctrl><hp:fieldEnd beginIDRef="55"/></hp:ctrl>"#),
            "fieldEnd must be emitted: {}",
            &xml[..500.min(xml.len())]
        );
        assert!(xml.contains("hello"), "text must still be present");

        // 순서 검증: fieldBegin < fieldEnd < "hello"
        let begin_pos = xml.find("fieldBegin").expect("fieldBegin");
        let end_pos = xml.find("fieldEnd").expect("fieldEnd");
        let hello_pos = xml.find("hello").expect("hello");
        assert!(begin_pos < end_pos, "fieldBegin must precede fieldEnd");
        assert!(
            end_pos < hello_pos,
            "fieldEnd must precede text for 0-length field"
        );
    }

    #[test]
    fn task1298_zero_length_field_mid_text() {
        // 0-length 필드 at position 3 (start=3, end=3), text="ABCDE":
        // HWP stream: A B C fieldBegin(8cu) fieldEnd(8cu) D E para_end
        // char_offsets: [0,1,2, 19,20] (D 앞에 16cu 갭)
        let mut f = Field::default();
        f.field_type = FieldType::ClickHere;
        f.field_id = 77;

        let mut para = Paragraph::default();
        para.text = "ABCDE".to_string();
        para.char_count = 5 + 8 + 8 + 1; // text + fieldBegin + fieldEnd + para_end
        para.char_offsets = vec![0, 1, 2, 19, 20];
        para.controls.push(Control::Field(f));
        para.field_ranges.push(FieldRange {
            start_char_idx: 3,
            end_char_idx: 3, // 0-length mid-text
            control_idx: 0,
        });

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            xml.contains("ABCDE") || (xml.contains("ABC") && xml.contains("DE")),
            "all text must be present: {}",
            &xml[..500.min(xml.len())]
        );

        // 순서 검증: "ABC" < fieldBegin < fieldEnd < "DE"
        let begin_pos = xml.find("fieldBegin").expect("fieldBegin");
        let end_pos = xml.find("fieldEnd").expect("fieldEnd");
        // ABC는 fieldBegin 앞에
        let abc_pos = xml.find('A').expect("A");
        // DE는 fieldEnd 뒤에 (fieldEnd 태그 닫힘 이후)
        let field_end_close =
            xml.find("fieldEnd").unwrap() + xml[xml.find("fieldEnd").unwrap()..].find('>').unwrap();
        let de_pos = xml[field_end_close..]
            .find('D')
            .map(|p| p + field_end_close)
            .expect("D after fieldEnd");

        assert!(abc_pos < begin_pos, "ABC must precede fieldBegin");
        assert!(begin_pos < end_pos, "fieldBegin must precede fieldEnd");
        assert!(end_pos < de_pos, "fieldEnd must precede DE");
    }

    // ---------- #1321: 빈 문단(text == "")의 0-length field 순서 ----------

    #[test]
    fn task1321_zero_length_field_in_empty_paragraph() {
        // 빈 문단(text="")에 0-length 필드:
        // HWP stream: fieldBegin(8cu) + fieldEnd(8cu) + para_end(1cu) = 17cu
        let mut f = Field::default();
        f.field_type = FieldType::ClickHere;
        f.field_id = 99;

        let mut para = Paragraph::default();
        para.text = "".to_string();
        para.char_count = 17;
        para.char_offsets = vec![];
        para.controls.push(Control::Field(f));
        para.field_ranges.push(FieldRange {
            start_char_idx: 0,
            end_char_idx: 0,
            control_idx: 0,
        });

        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();

        assert!(
            xml.contains(r#"<hp:fieldBegin id="99""#),
            "fieldBegin must be emitted: {}",
            &xml[..400.min(xml.len())]
        );
        assert!(
            xml.contains(r#"<hp:fieldEnd beginIDRef="99"/>"#),
            "fieldEnd must be emitted: {}",
            &xml[..400.min(xml.len())]
        );

        let begin_pos = xml.find("fieldBegin").expect("fieldBegin");
        let end_pos = xml.find("fieldEnd").expect("fieldEnd");
        assert!(
            begin_pos < end_pos,
            "빈 문단에서도 fieldBegin이 fieldEnd보다 앞에 와야 한다: {}",
            &xml[..400.min(xml.len())]
        );
    }

    // ---------- #1378: char_shapes 경계 기준 다중 run 분할 ----------

    fn cs(start_pos: u32, char_shape_id: u32) -> CharShapeRef {
        CharShapeRef {
            start_pos,
            char_shape_id,
        }
    }

    fn runs_of(para: &Paragraph) -> String {
        let doc = Document::default();
        let mut ctx = SerializeContext::collect_from_document(&doc);
        render_runs(para, &mut ctx)
    }

    #[test]
    fn task1378_two_run_split_mid_text() {
        // 경계가 텍스트 중간 — 텍스트 분할 (경계 케이스 2)
        let mut para = Paragraph::default();
        para.text = "abcdef".to_string();
        para.char_shapes = vec![cs(0, 1), cs(3, 2)];
        assert_eq!(
            runs_of(&para),
            r#"<hp:run charPrIDRef="1"><hp:t>abc</hp:t></hp:run><hp:run charPrIDRef="2"><hp:t>def</hp:t></hp:run>"#
        );
    }

    #[test]
    fn task1378_boundary_at_slot_position_slot_in_new_run() {
        // 경계와 컨트롤 슬롯이 같은 위치 — 경계 먼저 cut, 컨트롤은 새 run 소속 (경계 케이스 1)
        // 스트림: "ab"(0..2) + equation 슬롯(2..10) + "cd"(10..12), 경계 pos=2
        let mut para = Paragraph::default();
        para.text = "abcd".to_string();
        para.char_offsets = vec![0, 1, 10, 11];
        para.char_count = 13; // 4 + 8(슬롯) + 1
        para.controls.push(Control::Equation(Box::default()));
        para.char_shapes = vec![cs(0, 1), cs(2, 2)];
        let xml = runs_of(&para);
        assert!(
            xml.starts_with(r#"<hp:run charPrIDRef="1"><hp:t>ab</hp:t></hp:run><hp:run charPrIDRef="2"><hp:equation"#),
            "슬롯 위치의 경계는 슬롯보다 먼저 적용돼야 한다: {}",
            &xml[..200.min(xml.len())]
        );
        let run2 = xml.find(r#"<hp:run charPrIDRef="2">"#).unwrap();
        let cd = xml.find("<hp:t>cd</hp:t>").expect("cd 텍스트");
        assert!(run2 < cd, "cd 는 새 run 소속이어야 한다");
    }

    #[test]
    fn task1378_boundary_between_slot_and_text() {
        // 슬롯 이후 텍스트 중간 경계 — 슬롯은 이전 run, 분할은 텍스트에서
        // 스트림: equation 슬롯(0..8) + "ab"(8..10) + "cd"(10..12), 경계 pos=10
        let mut para = Paragraph::default();
        para.text = "abcd".to_string();
        para.char_offsets = vec![8, 9, 10, 11];
        para.char_count = 13;
        para.controls.push(Control::Equation(Box::default()));
        para.char_shapes = vec![cs(0, 1), cs(10, 2)];
        let xml = runs_of(&para);
        let run1_end = xml.find("</hp:run>").unwrap();
        let eq = xml.find("<hp:equation").expect("equation");
        let ab = xml.find("<hp:t>ab</hp:t>").expect("ab");
        assert!(
            eq < run1_end && ab < run1_end,
            "equation 과 ab 는 첫 run 소속"
        );
        assert!(
            xml.ends_with(r#"<hp:run charPrIDRef="2"><hp:t>cd</hp:t></hp:run>"#),
            "cd 만 새 run 으로 분할: {}",
            xml
        );
    }

    #[test]
    fn task1378_consecutive_same_id_boundary_skipped() {
        // 연속 동일 id 경계 skip — 파서 dedup 왕복 정합 (경계 케이스 3)
        let mut para = Paragraph::default();
        para.text = "abcdef".to_string();
        para.char_shapes = vec![cs(0, 5), cs(2, 5), cs(4, 6)];
        let xml = runs_of(&para);
        assert_eq!(
            xml,
            r#"<hp:run charPrIDRef="5"><hp:t>abcd</hp:t></hp:run><hp:run charPrIDRef="6"><hp:t>ef</hp:t></hp:run>"#,
            "동일 id 경계 (2,5) 는 skip 되고 (4,6) 만 분할해야 한다"
        );
    }

    #[test]
    fn task1378_tab_and_linebreak_in_split_runs() {
        // 탭(8 유닛)/lineBreak 를 포함한 run 분할 (경계 케이스 4)
        // 위치: a=0, \t=1..9, b=9, \n=10, c=11 — 경계 pos=9
        let mut para = Paragraph::default();
        para.text = "a\tb\nc".to_string();
        para.char_shapes = vec![cs(0, 1), cs(9, 2)];
        let xml = runs_of(&para);
        assert_eq!(
            xml,
            concat!(
                r#"<hp:run charPrIDRef="1"><hp:t>a<hp:tab width="4000" leader="0" type="1"/></hp:t></hp:run>"#,
                r#"<hp:run charPrIDRef="2"><hp:t>b<hp:lineBreak/>c</hp:t></hp:run>"#
            )
        );
    }

    #[test]
    fn task1378_empty_paragraph_single_run_id_zero() {
        // 빈 문단·char_shapes 빈 경우 — 단일 run id 0 유지 (경계 케이스 5)
        let para = Paragraph::default();
        assert_eq!(
            runs_of(&para),
            r#"<hp:run charPrIDRef="0"><hp:t></hp:t></hp:run>"#
        );
    }

    #[test]
    fn task1378_field_end_stays_in_previous_run() {
        // 필드 begin/end 와 경계 교차 (경계 케이스 6)
        // 스트림: fieldBegin(0..8) + "abc"(8..11) + fieldEnd(11..19) + "de"(19..21)
        // 경계 pos=19 — fieldEnd 는 이전 run, "de" 는 새 run
        let mut f = Field::default();
        f.field_type = FieldType::ClickHere;
        f.field_id = 11;
        let mut para = Paragraph::default();
        para.text = "abcde".to_string();
        para.char_offsets = vec![8, 9, 10, 19, 20];
        para.char_count = 22;
        para.controls.push(Control::Field(f));
        para.field_ranges.push(FieldRange {
            start_char_idx: 0,
            end_char_idx: 3,
            control_idx: 0,
        });
        para.char_shapes = vec![cs(0, 1), cs(19, 2)];
        let xml = runs_of(&para);
        let end_pos = xml.find("fieldEnd").expect("fieldEnd");
        let run2 = xml
            .find(r#"<hp:run charPrIDRef="2">"#)
            .expect("두 번째 run");
        assert!(
            end_pos < run2,
            "fieldEnd 는 이전 run 소속이어야 한다: {}",
            xml
        );
        assert!(
            xml.ends_with(r#"<hp:run charPrIDRef="2"><hp:t>de</hp:t></hp:run>"#),
            "de 는 새 run 소속이어야 한다: {}",
            xml
        );
    }

    #[test]
    fn task1378_trailing_boundary_emits_empty_run() {
        // 콘텐츠 끝 이후 경계 — 빈 run(<hp:t></hp:t>)으로 entry 보존 (규칙 5)
        let mut para = Paragraph::default();
        para.text = "abc".to_string();
        para.char_shapes = vec![cs(0, 1), cs(3, 2)];
        assert_eq!(
            runs_of(&para),
            r#"<hp:run charPrIDRef="1"><hp:t>abc</hp:t></hp:run><hp:run charPrIDRef="2"><hp:t></hp:t></hp:run>"#
        );
    }

    #[test]
    fn task1378_trailing_slot_in_new_run() {
        // 문단 끝 슬롯 위치의 경계 — trailing 슬롯도 새 run 소속 (규칙 1)
        // 스트림: "abc"(0..3) + equation 슬롯(3..11), 경계 pos=3
        let mut para = Paragraph::default();
        para.text = "abc".to_string();
        para.char_offsets = vec![0, 1, 2];
        para.char_count = 12; // 3 + 8(슬롯) + 1
        para.controls.push(Control::Equation(Box::default()));
        para.char_shapes = vec![cs(0, 1), cs(3, 2)];
        let xml = runs_of(&para);
        assert!(
            xml.starts_with(
                r#"<hp:run charPrIDRef="1"><hp:t>abc</hp:t></hp:run><hp:run charPrIDRef="2"><hp:equation"#
            ),
            "trailing 슬롯은 경계 적용 후 새 run 에 들어가야 한다: {}",
            &xml[..200.min(xml.len())]
        );
    }

    #[test]
    fn task1378_section_first_paragraph_secpr_run_id_follows_first_cs() {
        // 섹션 첫 문단 — secPr run id 와 텍스트 run id 의 dedup 상호작용 (경계 케이스 7)
        let mut para = Paragraph::default();
        para.text = "hi".to_string();
        para.char_shapes = vec![cs(0, 7)];
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        assert!(
            xml.contains(r#"<hp:run charPrIDRef="7"><hp:secPr "#),
            "secPr run id 는 첫 텍스트 run id 와 일치해야 한다 (재파싱 시 (0,0) 오염 방지)"
        );
        assert!(
            xml.contains(r#"<hp:run charPrIDRef="7"><hp:t>hi</hp:t></hp:run>"#),
            "텍스트 run 은 완전한 run 시퀀스로 치환돼야 한다"
        );
        assert!(
            !xml.contains(r#"<hp:run charPrIDRef="0">"#),
            "템플릿의 charPrIDRef=0 run 이 남아있으면 안 된다: {}",
            xml
        );
    }

    #[test]
    fn task1378_serialize_parse_roundtrip_preserves_char_shapes() {
        // serialize → parse 왕복 후 본문 char_shapes 시퀀스 보존.
        // 파서 정합 IR 로 구성: 섹션 첫 문단은 secPr(8)+colPr(8) 가 위치 축을 16 만큼
        // 선점하므로 char_offsets 가 16 부터 시작하고, 첫 entry 는 secPr run 시작(0)에
        // 기록된다 (stage1 양상 ② 메커니즘).
        use crate::model::style::CharShape;

        // 첫 문단: "abcdef", 경계 1개 — 원본 파스 결과는 [(0,1),(19,2)]
        let mut p0 = Paragraph::default();
        p0.text = "abcdef".to_string();
        p0.char_offsets = vec![16, 17, 18, 19, 20, 21];
        p0.char_count = 23;
        p0.char_shapes = vec![cs(0, 1), cs(19, 2)];

        // 추가 문단: 위치 축이 0 부터 — [(0,1),(3,2)]
        let mut p1 = Paragraph::default();
        p1.text = "abcdef".to_string();
        p1.char_offsets = vec![0, 1, 2, 3, 4, 5];
        p1.char_count = 7;
        p1.char_shapes = vec![cs(0, 1), cs(3, 2)];

        let mut section = Section::default();
        section.paragraphs.push(p0);
        section.paragraphs.push(p1);
        let mut doc = Document::default();
        doc.doc_info.char_shapes = vec![
            CharShape::default(),
            CharShape::default(),
            CharShape::default(),
        ];
        doc.sections.push(section);

        let bytes = crate::serializer::hwpx::serialize_hwpx(&doc).expect("serialize");
        let doc2 = crate::parser::hwpx::parse_hwpx(&bytes).expect("parse");
        let shapes_of = |i: usize| -> Vec<(u32, u32)> {
            doc2.sections[0].paragraphs[i]
                .char_shapes
                .iter()
                .map(|r| (r.start_pos, r.char_shape_id))
                .collect()
        };
        assert_eq!(shapes_of(0), vec![(0, 1), (19, 2)], "섹션 첫 문단");
        assert_eq!(shapes_of(1), vec![(0, 1), (3, 2)], "추가 문단");
    }
}
