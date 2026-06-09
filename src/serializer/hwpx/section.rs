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
    AutoNumber, AutoNumberType, Bookmark, Control, Equation, NewNumber, PageHide, PageNumberPos,
};
use crate::model::document::{Document, Section};
use crate::model::footnote::{Endnote, Footnote};
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
const TEXT_SLOT: &str = "<hp:t/>";
const LINESEG_SLOT_OPEN: &str = "<hp:linesegarray>";
const LINESEG_SLOT_CLOSE: &str = "</hp:linesegarray>";
const PARA_CLOSE: &str = "</hp:p></hs:sec>";

// 템플릿 내 첫 <hp:p> 태그의 실제 문자열 (id="3121190098" 랜덤 해시 포함).
// 템플릿은 정적이므로 이 문자열이 고정 위치에 있음이 보장됨.
const TEMPLATE_FIRST_P_TAG: &str = r#"<hp:p id="3121190098" paraPrIDRef="0" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0">"#;
// 템플릿 내 본문 텍스트용 run. 앞의 secPr/colPr run과 구분되는 두 번째 run이다.
const TEMPLATE_TEXT_RUN: &str = r#"<hp:run charPrIDRef="0"><hp:t/></hp:run>"#;
const TEMPLATE_SECTION_RUN_OPEN: &str = r#"<hp:run charPrIDRef="0"><hp:secPr"#;

/// 레퍼런스 기준 줄 레이아웃 파라미터.
const VERT_STEP: u32 = 1600; // vertsize(1000) + spacing(600)
const LINE_FLAGS: u32 = LineSeg::TAG_SINGLE_SEGMENT_LINE;
const HORZ_SIZE: u32 = 42520;
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

    let mut out = EMPTY_SECTION_XML.replacen(TEMPLATE_TEXT_RUN, &first_body, 1);
    out = replace_first_linesegs(&out, &first_linesegs);
    out = replace_page_pr(&out, &section.section_def.page_def);
    out = replace_page_border_fills(&out, &section.section_def);
    if let Some(first_char_shape_id) = first_para.and_then(first_section_run_char_shape_id) {
        out = replace_first_section_run_char_shape(&out, first_char_shape_id);
    }

    // 첫 문단 `<hp:p>` 태그를 IR 기반 속성으로 교체
    if let Some(p) = first_para {
        let new_p_tag = render_hp_p_open(p, ctx.next_para_id());
        out = out.replacen(TEMPLATE_FIRST_P_TAG, &new_p_tag, 1);
    }

    // 추가 문단: `</hp:p></hs:sec>` 직전에 `<hp:p>` 요소를 삽입.
    if section.paragraphs.len() > 1 {
        let mut extra = String::new();
        for p in section.paragraphs.iter().skip(1) {
            let (runs, linesegs, advance) = render_paragraph_xml_parts(p, vert_cursor, ctx);
            vert_cursor = advance;
            extra.push_str(&render_hp_p_open(p, ctx.next_para_id()));
            extra.push_str(&runs);
            extra.push_str(r#"<hp:linesegarray>"#);
            extra.push_str(&linesegs);
            extra.push_str(r#"</hp:linesegarray></hp:p>"#);
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
        let linesegs = render_lineseg_array_from_ir(&para.line_segs);
        let vert_end = next_vert_cursor_from_ir(&para.line_segs, vert_start);
        (runs_xml, linesegs, vert_end)
    } else {
        let (linesegs, vert_end) = render_lineseg_array_fallback(&para.text, vert_start);
        (runs_xml, linesegs, vert_end)
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

/// IR의 Paragraph를 기반으로 `<hp:p>` 시작 태그를 생성.
///
/// `id` 는 문단 순서 기반(0, 1, 2, ...)로 할당한다. 한컴 샘플은 랜덤 해시도 쓰지만
/// 파서는 id 를 무시하므로 순차값으로 충분.
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

/// 문단 첫 run 의 charPrIDRef. IR의 `char_shapes[0].char_shape_id` 사용.
/// 비어있으면 0 (기본 글자모양) 반환.
pub(crate) fn first_run_char_shape_id(p: &Paragraph) -> u32 {
    p.char_shapes.first().map(|r| r.char_shape_id).unwrap_or(0)
}

/// Paragraph 하나를 (`<hp:run>` XML, lineseg XML, 다음 vert_cursor)로 변환한다.
pub(crate) fn render_paragraph_xml_parts(
    para: &Paragraph,
    vert_start: u32,
    ctx: &mut SerializeContext,
) -> (String, String, u32) {
    let runs_xml = render_paragraph_runs(para, ctx);

    if !para.line_segs.is_empty() {
        let linesegs = render_lineseg_array_from_ir(&para.line_segs);
        let vert_end = next_vert_cursor_from_ir(&para.line_segs, vert_start);
        (runs_xml, linesegs, vert_end)
    } else {
        let (linesegs, vert_end) = render_lineseg_array_fallback(&para.text, vert_start);
        (runs_xml, linesegs, vert_end)
    }
}

/// Paragraph 하나를 (`<hp:t>` XML, lineseg XML, 다음 vert_cursor)로 변환.
///
/// `<hp:lineseg>` 출력 원칙 (#177):
/// - `para.line_segs` 가 비어있지 않으면 **IR 값 그대로 출력**
/// - 비어있을 때만 텍스트 내 `\n` 기반으로 fallback 생성 (빈 문단·`Document::default()` 호환)
pub(crate) fn render_paragraph_parts(
    para: &Paragraph,
    vert_start: u32,
    ctx: &mut SerializeContext,
) -> (String, String, u32) {
    let t_xml = render_run_content(para, ctx);

    if !para.line_segs.is_empty() {
        // IR 기반 출력 — 원본 lineseg 값 보존 (#177)
        let linesegs = render_lineseg_array_from_ir(&para.line_segs);
        let vert_end = next_vert_cursor_from_ir(&para.line_segs, vert_start);
        (t_xml, linesegs, vert_end)
    } else {
        // Fallback — IR에 line_segs 가 없으면 기존 생성 로직 유지
        let (linesegs, vert_end) = render_lineseg_array_fallback(&para.text, vert_start);
        (t_xml, linesegs, vert_end)
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

    if can_render_char_shape_runs(para) {
        let runs = render_text_char_shape_runs(para, ctx);
        if !runs.is_empty() {
            return runs;
        }
    }

    let cs = first_run_char_shape_id(para);
    reference_char_shape_if_applicable(ctx, cs);
    format!(
        r#"<hp:run charPrIDRef="{}">{}</hp:run>"#,
        cs,
        render_run_content(para, ctx)
    )
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
    let auto_number_count = para
        .controls
        .iter()
        .filter(|control| matches!(control, Control::AutoNumber(_)))
        .count();
    let zero_width_slot_count = para.hwpx_zero_width_control_slots.len();
    if slot_count
        .saturating_add(auto_number_count)
        .saturating_add(zero_width_slot_count)
        != para.controls.len()
    {
        return None;
    }
    if !para.hwpx_zero_width_control_slots.iter().all(|slot| {
        matches!(
            para.controls.get(slot.control_idx),
            Some(Control::Bookmark(_))
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
            if is_hwpx_zero_width_span_slot(slots, *slot_idx, char_pos) {
                let slot = &slots[*slot_idx];
                if slot.pos >= start_pos && slot.pos < end_pos {
                    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
                    render_hwpx_run_slot(&mut out, &slot.kind, ctx);
                    *slot_idx += 1;
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
                if matches!(slot.kind, HwpxRunSlotKind::Control(Control::Bookmark(_))) {
                    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, tab_idx);
                    render_hwpx_run_slot(&mut out, &slot.kind, ctx);
                    *slot_idx += 1;
                    continue;
                }
                break;
            }
        }
        while *slot_idx < slots.len() && slots[*slot_idx].pos < end_pos {
            let slot = &slots[*slot_idx];
            let slot_pos = slot.pos;
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
        slot.pos == char_pos && matches!(slot.kind, HwpxRunSlotKind::Control(Control::Bookmark(_)))
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
    let (linesegs, vert_end) = render_lineseg_array_fallback(text, vert_start);
    (t_xml, linesegs, vert_end)
}

/// `<hp:t>...</hp:t>` 본문 생성 — 탭/소프트브레이크/XML escape 포함.
///
/// `tab_extended`: IR의 탭 확장 정보 목록. `tab_idx`를 통해 탭 문자마다 순서대로 참조.
/// 항목이 없으면 폴백(width=TAB_DEFAULT_WIDTH, leader=0, type=1)을 사용.
fn render_hp_t_content(text: &str, tab_extended: &[[u16; 7]], tab_idx: &mut usize) -> String {
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

/// Paragraph의 본문 run 콘텐츠를 `<hp:t>`와 인라인 컨트롤 XML로 직렬화한다.
fn render_run_content(para: &Paragraph, ctx: &mut SerializeContext) -> String {
    // Bookmark는 IR에 위치 정보가 없어 문단 시작에 배치한다.
    // (HWPX 파서가 char_count에 포함하지 않아 slot 시스템이 위치를 추적할 수 없음)
    let mut prefix = String::new();
    for ctrl in &para.controls {
        if let Control::Bookmark(bm) = ctrl {
            if let Ok(xml) = writer_to_string(|w| write_bookmark(w, bm)) {
                prefix.push_str("<hp:ctrl>");
                prefix.push_str(&xml);
                prefix.push_str("</hp:ctrl>");
            }
        }
    }

    let slot_count = inferred_control_slot_count(para);
    let slots: Vec<&Control> = if slot_count == para.controls.len() {
        para.controls.iter().collect()
    } else {
        para.controls
            .iter()
            .filter(|c| is_hwpx_fallback_inline_slot(c))
            .collect()
    };

    let mut tab_idx = 0usize;

    if slots.is_empty() && para.field_ranges.is_empty() {
        let mut out = prefix;
        out.push_str(&render_hp_t_content(
            &para.text,
            &para.tab_extended,
            &mut tab_idx,
        ));
        return out;
    }

    if slot_count != slots.len() {
        let mut out = prefix;
        out.push_str(&render_hp_t_content(
            &para.text,
            &para.tab_extended,
            &mut tab_idx,
        ));
        for slot in &slots {
            render_control_slot(&mut out, slot, ctx);
        }
        return out;
    }

    let mut out = prefix;
    let mut text_buf = String::new();
    let mut slot_idx = 0usize;
    let mut expected_utf16_pos = 0u32;
    let mut field_end_emitted = vec![false; para.field_ranges.len()];

    // 빈 문단(text == "")의 0-length 필드: 메인 루프가 실행되지 않아
    // pre-char 검사를 통과하지 못하므로 루프 전에 slots → fieldEnd 순으로 방출한다.
    if para.text.is_empty() {
        while slot_idx < slots.len() {
            render_control_slot(&mut out, slots[slot_idx], ctx);
            slot_idx += 1;
        }
        for (i, fr) in para.field_ranges.iter().enumerate() {
            if fr.start_char_idx == fr.end_char_idx && !field_end_emitted[i] {
                if let Some(Control::Field(f)) = para.controls.get(fr.control_idx) {
                    if let Ok(xml) = writer_to_string(|w| write_field_end(w, f.field_id)) {
                        out.push_str("<hp:ctrl>");
                        out.push_str(&xml);
                        out.push_str("</hp:ctrl>");
                    }
                }
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
            flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, &mut tab_idx);
            render_control_slot(&mut out, slots[slot_idx], ctx);
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
                flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, &mut tab_idx);
                if let Some(Control::Field(f)) = para.controls.get(fr.control_idx) {
                    if let Ok(xml) = writer_to_string(|w| write_field_end(w, f.field_id)) {
                        out.push_str("<hp:ctrl>");
                        out.push_str(&xml);
                        out.push_str("</hp:ctrl>");
                    }
                }
                field_end_emitted[i] = true;
            }
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
                flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, &mut tab_idx);
                if let Some(Control::Field(f)) = para.controls.get(fr.control_idx) {
                    if let Ok(xml) = writer_to_string(|w| write_field_end(w, f.field_id)) {
                        out.push_str("<hp:ctrl>");
                        out.push_str(&xml);
                        out.push_str("</hp:ctrl>");
                    }
                }
                field_end_emitted[i] = true;
            }
        }
    }

    flush_text_fragment(&mut out, &mut text_buf, &para.tab_extended, &mut tab_idx);

    // end_char_idx >= text.len() 인 경우 루프에서 감지되지 않으므로 루프 후에 처리
    for (i, fr) in para.field_ranges.iter().enumerate() {
        if !field_end_emitted[i] {
            if let Some(Control::Field(f)) = para.controls.get(fr.control_idx) {
                if let Ok(xml) = writer_to_string(|w| write_field_end(w, f.field_id)) {
                    out.push_str("<hp:ctrl>");
                    out.push_str(&xml);
                    out.push_str("</hp:ctrl>");
                }
            }
        }
    }

    while slot_idx < slots.len() {
        render_control_slot(&mut out, slots[slot_idx], ctx);
        slot_idx += 1;
    }

    if out.is_empty() {
        render_hp_t_content("", &para.tab_extended, &mut tab_idx)
    } else {
        out
    }
}

fn inferred_control_slot_count(para: &Paragraph) -> usize {
    let text_units: u32 = para.text.chars().map(char_utf16_width).sum();
    let from_char_count = para.char_count.saturating_sub(1).saturating_sub(text_units) / 8;

    let mut from_offsets = 0u32;
    let mut expected = 0u32;
    for (idx, c) in para.text.chars().enumerate() {
        let pos = para.char_offsets.get(idx).copied().unwrap_or(expected);
        if pos > expected {
            from_offsets += (pos - expected) / 8;
        }
        expected = pos.max(expected).saturating_add(char_utf16_width(c));
    }

    // fieldEnd는 8 code unit 슬롯이지만 para.controls[]에 대응 컨트롤이 없다.
    // field_ranges.len()이 fieldEnd 수와 정확히 일치하므로 빼서 보정한다.
    from_char_count
        .max(from_offsets)
        .saturating_sub(para.field_ranges.len() as u32) as usize
}

fn is_hwpx_inline_slot(control: &Control) -> bool {
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
        Control::Bookmark(bm) => out.push_str(&render_bookmark(bm)),
        Control::PageHide(ph) => out.push_str(&render_page_hiding(ph)),
        Control::PageNumberPos(pn) => out.push_str(&render_page_num(pn)),
        Control::NewNumber(nn) => out.push_str(&render_new_num(nn)),
        Control::ColumnDef(cd) => out.push_str(&render_column_def(cd)),
        Control::Header(h) => out.push_str(&render_header(h, ctx)),
        Control::Footer(f) => out.push_str(&render_footer(f, ctx)),
        Control::AutoNumber(an) => out.push_str(&render_autonum(an)),
        _ => {}
    }
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
    let mut out = format!(
        concat!(
            r#"<hp:ctrl><hp:{tag} id="{id}" applyPageType="{apply}">"#,
            r#"<hp:subList id="" textDirection="HORIZONTAL" lineWrap="BREAK" vertAlign="TOP" "#,
            r#"linkListIDRef="0" linkListNextIDRef="0" textWidth="{tw}" textHeight="{th}" "#,
            r#"hasTextRef="{tr}" hasNumRef="{nr}">"#
        ),
        tag = tag,
        id = h.id.unwrap_or(0),
        apply = apply_page_type_to_str(h.apply_to),
        tw = h.text_width,
        th = h.text_height,
        tr = h.text_ref,
        nr = h.num_ref,
    );
    let mut vert_cursor: u32 = 0;
    for p in h.paragraphs.iter() {
        let (runs, linesegs, advance) = render_paragraph_xml_parts(p, vert_cursor, ctx);
        vert_cursor = advance;
        out.push_str(&render_hp_p_open(p, ctx.next_para_id()));
        out.push_str(&runs);
        out.push_str(r#"<hp:linesegarray>"#);
        out.push_str(&linesegs);
        out.push_str(r#"</hp:linesegarray></hp:p>"#);
    }
    out.push_str(&format!("</hp:subList></hp:{tag}></hp:ctrl>", tag = tag));
    out
}

/// render_header_footer 공통 인자 묶음 (Header/Footer가 동일 필드를 가짐).
struct HeaderFooterFields<'a> {
    id: Option<u32>,
    apply_to: HeaderFooterApply,
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

/// 번호 종류 → HWPX `numType` 문자열. `parse_num_type`의 역매핑(Picture→FIGURE).
fn auto_number_type_to_str(t: AutoNumberType) -> &'static str {
    match t {
        AutoNumberType::Page => "PAGE",
        AutoNumberType::Footnote => "FOOTNOTE",
        AutoNumberType::Endnote => "ENDNOTE",
        AutoNumberType::Picture => "FIGURE",
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

fn render_shape(shape: &ShapeObject, ctx: &SerializeContext) -> String {
    // Rectangle: Writer-based serializer (drawText 포함)
    if let ShapeObject::Rectangle(r) = shape {
        return match writer_to_string(|w| super::shape::write_rect(w, r)) {
            Ok(xml) => xml,
            Err(e) => {
                eprintln!("[hwpx] Shape::Rectangle 직렬화 실패: {e}");
                String::new()
            }
        };
    }
    // Line: Writer-based serializer
    if let ShapeObject::Line(l) = shape {
        return match writer_to_string(|w| super::shape::write_line(w, l)) {
            Ok(xml) => xml,
            Err(e) => {
                eprintln!("[hwpx] Shape::Line 직렬화 실패: {e}");
                String::new()
            }
        };
    }
    let (tag, c) = match shape {
        ShapeObject::Rectangle(_) | ShapeObject::Line(_) => unreachable!(),
        ShapeObject::Ellipse(e) => ("ellipse", &e.common),
        ShapeObject::Arc(a) => ("arc", &a.common),
        ShapeObject::Polygon(p) => ("polygon", &p.common),
        ShapeObject::Curve(cv) => ("curve", &cv.common),
        ShapeObject::Group(g) => ("container", &g.common),
        ShapeObject::Picture(pic) => {
            return match writer_to_string(|w| picture::write_picture(w, pic, ctx)) {
                Ok(xml) => xml,
                Err(e) => {
                    eprintln!("[hwpx] Shape::Picture 직렬화 실패: {e}");
                    String::new()
                }
            };
        }
        ShapeObject::Chart(ch) => ("chart", &ch.common),
        ShapeObject::Ole(o) => ("ole", &o.common),
    };
    render_common_shape_xml(tag, c)
}

fn render_common_shape_xml(tag: &str, c: &CommonObjAttr) -> String {
    format!(
        concat!(
            r#"<hp:{tag} id="{id}" zOrder="{zo}" textWrap="{tw}" textFlow="BOTH_SIDES" lock="0">"#,
            r#"<hp:sz width="{w}" height="{h}" widthRelTo="ABSOLUTE" heightRelTo="ABSOLUTE"/>"#,
            r#"<hp:pos treatAsChar="{tac}" vertRelTo="{vr}" vertAlign="{va}" horzRelTo="{hr}" horzAlign="{ha}" vertOffset="{vo}" horzOffset="{ho}"/>"#,
            r#"<hp:outMargin left="{ml}" right="{mr}" top="{mt}" bottom="{mb}"/>"#,
            r#"</hp:{tag}>"#,
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
    )
}

fn render_note_sublist(
    tag: &str,
    number: u16,
    paragraphs: &[Paragraph],
    ctx: &mut SerializeContext,
) -> String {
    let mut out = format!(
        r#"<hp:ctrl><hp:{tag} number="{num}"><hp:subList id="" textDirection="HORIZONTAL" lineWrap="BREAK" vertAlign="TOP" linkListIDRef="0" linkListNextIDRef="0" textWidth="0" textHeight="0" hasTextRef="0" hasNumRef="0">"#,
        tag = tag,
        num = number,
    );
    let mut vert_cursor: u32 = 0;
    for p in paragraphs.iter() {
        let (runs, linesegs, advance) = render_paragraph_xml_parts(p, vert_cursor, ctx);
        vert_cursor = advance;
        out.push_str(&render_hp_p_open(p, ctx.next_para_id()));
        out.push_str(&runs);
        out.push_str(r#"<hp:linesegarray>"#);
        out.push_str(&linesegs);
        out.push_str(r#"</hp:linesegarray></hp:p>"#);
    }
    out.push_str(&format!("</hp:subList></hp:{tag}></hp:ctrl>", tag = tag));
    out
}

fn render_footnote(note: &Footnote, ctx: &mut SerializeContext) -> String {
    render_note_sublist("footNote", note.number, &note.paragraphs, ctx)
}

fn render_endnote(note: &Endnote, ctx: &mut SerializeContext) -> String {
    render_note_sublist("endNote", note.number, &note.paragraphs, ctx)
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
/// 과거 동작을 보존하기 위해 기존 정적값으로 lineseg 생성.
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

fn flush_buf(t_xml: &mut String, buf: &mut String) {
    if !buf.is_empty() {
        t_xml.push_str(&xml_escape(buf));
        buf.clear();
    }
}

/// Fallback 전용 static lineseg 생성기 — IR에 값이 없을 때만 사용.
/// 주: 이 함수의 출력은 "명세 상 정확한 값" 이 아닌 정적 자리표이므로,
/// 호출 후 문서는 `DocumentCore::from_bytes` 의 `reflow_zero_height_paragraphs`
/// 또는 사용자의 `reflow_linesegs_on_demand` 로 재계산되어야 한다.
fn push_lineseg_static(out: &mut String, textpos: u32, vertpos: u32) {
    out.push_str(&format!(
        r#"<hp:lineseg textpos="{}" vertpos="{}" vertsize="1000" textheight="1000" baseline="850" spacing="600" horzpos="0" horzsize="{}" flags="{}"/>"#,
        textpos, vertpos, HORZ_SIZE, LINE_FLAGS,
    ));
}

fn replace_first_linesegs(xml: &str, new_inner: &str) -> String {
    let open = xml
        .find(LINESEG_SLOT_OPEN)
        .expect("template has linesegarray");
    let inner_start = open + LINESEG_SLOT_OPEN.len();
    let close_rel = xml[inner_start..]
        .find(LINESEG_SLOT_CLOSE)
        .expect("template has closing linesegarray");
    let inner_end = inner_start + close_rel;
    let mut out = String::with_capacity(xml.len() + new_inner.len());
    out.push_str(&xml[..inner_start]);
    out.push_str(new_inner);
    out.push_str(&xml[inner_end..]);
    out
}

/// [#1166] 템플릿 pagePr 의 고정 용지 속성(landscape/width/height)을 IR page_def
/// 값으로 치환한다. 종전엔 템플릿 하드코딩값(landscape="WIDELY" width=59528
/// height=84186)이 그대로 출력되어 HWPX 저장 시 가로/세로 + 용지 크기가 손실됐다.
///
/// OWPML landscape: WIDELY=세로(landscape=false), NARROWLY=가로(landscape=true).
/// width/height 는 짧은변/긴변 그대로 (HWP 바이너리 동일 규약).
fn replace_page_pr(xml: &str, page_def: &crate::model::page::PageDef) -> String {
    // 템플릿의 pagePr 여는 태그(고정 문자열) → IR 기반으로 교체.
    const TEMPLATE_PAGE_PR: &str =
        r#"<hp:pagePr landscape="WIDELY" width="59528" height="84186" gutterType="LEFT_ONLY">"#;
    const TEMPLATE_MARGIN: &str = r#"<hp:margin header="4252" footer="4252" gutter="0" left="8504" right="8504" top="5668" bottom="4252"/>"#;
    let landscape = if page_def.landscape {
        "NARROWLY"
    } else {
        "WIDELY"
    };
    let new_page_pr = format!(
        r#"<hp:pagePr landscape="{}" width="{}" height="{}" gutterType="LEFT_ONLY">"#,
        landscape, page_def.width, page_def.height,
    );
    let out = if xml.contains(TEMPLATE_PAGE_PR) {
        xml.replacen(TEMPLATE_PAGE_PR, &new_page_pr, 1)
    } else {
        // 템플릿이 변경됐거나 이미 치환된 경우 — 원본 유지(회귀 방지).
        xml.to_string()
    };
    if out.contains(TEMPLATE_MARGIN) {
        out.replacen(TEMPLATE_MARGIN, &render_page_margin(page_def), 1)
    } else {
        out
    }
}

const TEMPLATE_PAGE_BORDER_FILLS: &str = concat!(
    r#"<hp:pageBorderFill type="BOTH" borderFillIDRef="1" textBorder="PAPER" headerInside="0" footerInside="0" fillArea="PAPER"><hp:offset left="1417" right="1417" top="1417" bottom="1417"/></hp:pageBorderFill>"#,
    r#"<hp:pageBorderFill type="EVEN" borderFillIDRef="1" textBorder="PAPER" headerInside="0" footerInside="0" fillArea="PAPER"><hp:offset left="1417" right="1417" top="1417" bottom="1417"/></hp:pageBorderFill>"#,
    r#"<hp:pageBorderFill type="ODD" borderFillIDRef="1" textBorder="PAPER" headerInside="0" footerInside="0" fillArea="PAPER"><hp:offset left="1417" right="1417" top="1417" bottom="1417"/></hp:pageBorderFill>"#,
);

fn render_page_margin(page_def: &crate::model::page::PageDef) -> String {
    format!(
        r#"<hp:margin header="{}" footer="{}" gutter="{}" left="{}" right="{}" top="{}" bottom="{}"/>"#,
        page_def.margin_header,
        page_def.margin_footer,
        page_def.margin_gutter,
        page_def.margin_left,
        page_def.margin_right,
        page_def.margin_top,
        page_def.margin_bottom,
    )
}

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

        assert_eq!(
            xml.matches(r#"<hp:run charPrIDRef="7">"#).count(),
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

        assert_eq!(
            xml.matches(r#"<hp:run charPrIDRef="7">"#).count(),
            2,
            "field boundary runs should survive: {}",
            xml
        );
        assert_eq!(
            xml.matches(r#"<hp:run charPrIDRef="8">"#).count(),
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
            .find(r#"<hp:run charPrIDRef="7">"#)
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
    fn task177_fallback_used_when_ir_empty() {
        // IR 의 line_segs 가 비어있으면 fallback 경로로 정적 값 출력.
        let mut para = Paragraph::default();
        para.text = "a\nb".to_string(); // 소프트브레이크 1개 → fallback 은 lineseg 2개 생성
        let (doc, section) = make_doc_with_paragraph(para);
        let mut ctx = SerializeContext::collect_from_document(&doc);
        let xml = String::from_utf8(write_section(&section, &doc, 0, &mut ctx).unwrap()).unwrap();
        // 정적 fallback: vertsize=1000, textheight=1000, baseline=850, spacing=600
        assert!(xml.contains(r#"vertsize="1000""#));
        assert!(xml.contains(r#"baseline="850""#));
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

    use crate::model::control::{Bookmark, Control, Field, FieldType};
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
}
