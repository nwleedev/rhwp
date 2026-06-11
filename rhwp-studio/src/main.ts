import { WasmBridge } from '@/core/wasm-bridge';
import type { DocumentInfo } from '@/core/types';
import { EventBus } from '@/core/event-bus';
import { CanvasView } from '@/view/canvas-view';
import { InputHandler } from '@/engine/input-handler';
import { DeleteTextCommand, InsertTextCommand, type EditCommand } from '@/engine/command';
import { Toolbar } from '@/ui/toolbar';
import { MenuBar } from '@/ui/menu-bar';
import { loadWebFonts } from '@/core/font-loader';
import { CommandRegistry } from '@/command/registry';
import { CommandDispatcher } from '@/command/dispatcher';
import type { EditorContext, CommandServices } from '@/command/types';
import { confirmSaveBeforeReplacingDocument, fileCommands } from '@/command/commands/file';
import { editCommands } from '@/command/commands/edit';
import { viewCommands } from '@/command/commands/view';
import { formatCommands } from '@/command/commands/format';
import { insertCommands } from '@/command/commands/insert';
import { tableCommands } from '@/command/commands/table';
import { pageCommands } from '@/command/commands/page';
import { toolCommands } from '@/command/commands/tool';
import { installPwaFileHandling, type FileHandlingWindowLike } from '@/command/pwa-file-handling';
import { ContextMenu } from '@/ui/context-menu';
import { CommandPalette } from '@/ui/command-palette';
import { showValidationModalIfNeeded } from '@/ui/validation-modal';
import { showToast } from '@/ui/toast';
import { initRhwpDev } from '@/core/rhwp-dev';
import { DocumentDirtyState } from '@/core/document-dirty-state';
import { CellSelectionRenderer } from '@/engine/cell-selection-renderer';
import { TableObjectRenderer } from '@/engine/table-object-renderer';
import { TableResizeRenderer } from '@/engine/table-resize-renderer';
import { Ruler } from '@/view/ruler';
import type { CanvasKitLayerRenderer } from '@/view/canvaskit-renderer';
import {
  resolveCanvasKitRenderMode,
  resolveCanvasKitSurfaceRequest,
  resolveRenderBackendRequest,
  resolveRenderProfile,
} from '@/view/render-backend';

const wasm = new WasmBridge();
const eventBus = new EventBus();
const documentState = new DocumentDirtyState(eventBus);
documentState.installBeforeUnload(window);

const runtimeIdentity = {
  schemaVersion: 1,
  rhwpStudioVersion: __APP_VERSION__,
  sourceCommit: __RHWP_SOURCE_COMMIT__,
  patchId: __RHWP_PATCH_ID__,
  buildId: __RHWP_BUILD_ID__,
  basePath: __RHWP_STUDIO_BASE_PATH__,
  pwaCleanupExpected: __RHWP_PWA_CLEANUP_EXPECTED__,
};

type UnsupportedFingerprintGetter = {
  path: string;
  reason: string;
};

type FormattingFingerprintParams = {
  pageStart?: number;
  pageEnd?: number;
  includeLayerTree?: boolean;
};

type DeterministicEditParams = {
  text?: unknown;
  sectionIndex?: unknown;
  paragraphIndex?: unknown;
  charOffset?: unknown;
};

type UndoRedoCaptureProofParams = DeterministicEditParams;
type RecordOnlyCaptureProofParams = DeterministicEditParams;
type ImeCompositionCaptureProofParams = DeterministicEditParams;
type IosFallbackInputCaptureProofParams = DeterministicEditParams & {
  intermediateText?: unknown;
};
type ImagePasteCaptureProofParams = DeterministicEditParams;
type ControlPasteCaptureProofParams = DeterministicEditParams & {
  controlIndex?: unknown;
  parentParaIndex?: unknown;
};
type InternalPasteCaptureProofParams = DeterministicEditParams & {
  copyLength?: unknown;
};

type BodyParagraphTarget = {
  paragraphIndex: number;
  paragraphLength: number;
};

type BodyControlTarget = {
  controlIndex: number;
  pageIndex: number;
  parentParaIndex: number;
  sectionIndex: number;
  type: string;
};

type TableCellTextTarget = {
  cellIndex: number;
  cellParaIndex: number;
  charLength: number;
  col?: number;
  controlIndex: number;
  pageIndex: number;
  parentParaIndex: number;
  row?: number;
  sectionIndex: number;
  value: string;
};

type TableCellTextParams = {
  cellIndex?: unknown;
  cellParaIndex?: unknown;
  controlIndex?: unknown;
  parentParaIndex?: unknown;
  sectionIndex?: unknown;
  text?: unknown;
};

type TableCellResizeParams = {
  cellIndex?: unknown;
  controlIndex?: unknown;
  heightDelta?: unknown;
  parentParaIndex?: unknown;
  sectionIndex?: unknown;
  widthDelta?: unknown;
};

type FootnoteTextTarget = {
  charLength: number;
  controlIndex: number;
  footnoteIndex: number;
  footnoteNumber: number;
  fnParaIndex: number;
  pageIndex: number;
  parentParaIndex: number;
  sectionIndex: number;
  sourceType: string;
  value: string;
};

type FootnoteTextParams = {
  controlIndex?: unknown;
  fnParaIndex?: unknown;
  parentParaIndex?: unknown;
  sectionIndex?: unknown;
  text?: unknown;
};

type ClickHerePropsParams = {
  editable?: unknown;
  fieldId?: unknown;
  guide?: unknown;
  memo?: unknown;
  name?: unknown;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function boundedPageRange(params: FormattingFingerprintParams | undefined, pageCount: number): { start: number; end: number } {
  const requestedStart = Number.isFinite(params?.pageStart) ? Math.trunc(Number(params?.pageStart)) : 0;
  const requestedEnd = Number.isFinite(params?.pageEnd) ? Math.trunc(Number(params?.pageEnd)) : pageCount - 1;
  const start = Math.max(0, Math.min(requestedStart, Math.max(0, pageCount - 1)));
  const end = Math.max(start, Math.min(requestedEnd, Math.max(0, pageCount - 1)));
  return { start, end };
}

function incrementCounter(target: Record<string, number>, key: unknown): void {
  if (typeof key !== 'string' && typeof key !== 'number' && typeof key !== 'boolean') return;
  const normalized = String(key);
  target[normalized] = (target[normalized] ?? 0) + 1;
}

function summarizeControls(layout: { controls?: unknown[] } | null | undefined): Record<string, unknown> {
  const controls = Array.isArray(layout?.controls) ? layout.controls as Array<Record<string, unknown>> : [];
  const byType: Record<string, number> = {};
  const byWrap: Record<string, number> = {};
  const byPlane: Record<string, number> = {};
  const bounds = controls.map((control) => ({
    type: control.type,
    x: control.x,
    y: control.y,
    w: control.w,
    h: control.h,
    plane: control.plane,
    zOrder: control.zOrder,
    stableIndex: control.stableIndex,
    wrap: control.wrap,
    secIdx: control.secIdx,
    paraIdx: control.paraIdx,
    controlIdx: control.controlIdx,
    cellIdx: control.cellIdx,
    cellParaIdx: control.cellParaIdx,
    headerFooter: control.headerFooter,
    noteRef: control.noteRef,
  }));

  for (const control of controls) {
    incrementCounter(byType, control.type);
    incrementCounter(byWrap, control.wrap);
    incrementCounter(byPlane, control.plane);
  }

  return {
    count: controls.length,
    byType,
    byWrap,
    byPlane,
    bounds,
  };
}

function summarizeLayerTree(layerTree: unknown): Record<string, unknown> {
  const counts = {
    arrays: 0,
    ops: 0,
    byKind: {} as Record<string, number>,
    byType: {} as Record<string, number>,
    byWrap: {} as Record<string, number>,
    byProfile: {} as Record<string, number>,
  };

  const walk = (value: unknown): void => {
    if (Array.isArray(value)) {
      counts.arrays += 1;
      for (const item of value) walk(item);
      return;
    }
    if (!value || typeof value !== 'object') return;

    const objectValue = value as Record<string, unknown>;
    incrementCounter(counts.byKind, objectValue.kind);
    incrementCounter(counts.byType, objectValue.type);
    incrementCounter(counts.byWrap, objectValue.wrap);
    incrementCounter(counts.byProfile, objectValue.profile);
    if (Array.isArray(objectValue.ops)) counts.ops += objectValue.ops.length;

    for (const [key, nested] of Object.entries(objectValue)) {
      if (key === 'data' || key === 'base64' || key === 'text') continue;
      walk(nested);
    }
  };

  walk(layerTree);
  return counts;
}

function summarizeHeaderFooter(sectionIndex: number, isHeader: boolean, applyTo: number): Record<string, unknown> {
  const raw = JSON.parse(wasm.getHeaderFooter(sectionIndex, isHeader, applyTo)) as Record<string, unknown>;
  if (!raw.exists) return raw;
  const text = typeof raw.text === 'string' ? raw.text : '';
  const normalizedText = text.replace(/\s+/g, '');
  const summary: Record<string, unknown> = { ...raw };
  delete summary.text;
  summary.charCount = normalizedText.length;
  summary.lineCount = normalizedText.length === 0 ? 0 : text.split('\n').length;
  return summary;
}

function buildFormattingFingerprint(params?: FormattingFingerprintParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }

  const unsupported: UnsupportedFingerprintGetter[] = [];
  const documentInfo = wasm.getDocumentInfo();
  const sectionCount = wasm.getSectionCount();
  const pageCount = wasm.pageCount;
  const pageRange = boundedPageRange(params, pageCount);
  const includeLayerTree = params?.includeLayerTree !== false;

  const safe = <T>(path: string, getter: () => T): T | null => {
    try {
      return getter();
    } catch (error) {
      unsupported.push({ path, reason: errorMessage(error) });
      return null;
    }
  };

  const sections = [];
  for (let sectionIndex = 0; sectionIndex < sectionCount; sectionIndex += 1) {
    const headerFooter = [];
    for (const isHeader of [true, false]) {
      for (const applyTo of [0, 1, 2]) {
        const kind = isHeader ? 'header' : 'footer';
        const summary = safe(
          `sections[${sectionIndex}].${kind}[${applyTo}]`,
          () => summarizeHeaderFooter(sectionIndex, isHeader, applyTo),
        );
        headerFooter.push({ kind, applyTo, summary });
      }
    }
    sections.push({
      sectionIndex,
      pageDef: safe(`sections[${sectionIndex}].pageDef`, () => wasm.getPageDef(sectionIndex)),
      sectionDef: safe(`sections[${sectionIndex}].sectionDef`, () => wasm.getSectionDef(sectionIndex)),
      pageBorderFill: safe(`sections[${sectionIndex}].pageBorderFill`, () => wasm.getPageBorderFill(sectionIndex)),
      headerFooter,
    });
  }

  const pages = [];
  for (let pageIndex = pageRange.start; pageIndex <= pageRange.end; pageIndex += 1) {
    const controlLayout = safe(`pages[${pageIndex}].controlLayout`, () => wasm.getPageControlLayout(pageIndex));
    const layerTree = includeLayerTree
      ? safe(`pages[${pageIndex}].layerTree`, () => JSON.parse(wasm.getPageLayerTree(pageIndex)))
      : null;
    pages.push({
      pageIndex,
      pageInfo: safe(`pages[${pageIndex}].pageInfo`, () => wasm.getPageInfo(pageIndex)),
      controlLayoutSummary: summarizeControls(controlLayout),
      layerTreeSummary: includeLayerTree ? summarizeLayerTree(layerTree) : null,
    });
  }

  return {
    schemaVersion: 1,
    runtimeIdentity,
    sourceFormat: wasm.getSourceFormat(),
    document: documentInfo,
    coverage: {
      pageCount,
      sectionCount,
      requestedPageStart: params?.pageStart ?? null,
      requestedPageEnd: params?.pageEnd ?? null,
      actualPageStart: pageRange.start,
      actualPageEnd: pageRange.end,
      includeLayerTree,
    },
    sections,
    pages,
    unsupported,
  };
}

function boundedInteger(value: unknown, fallback: number, min: number, max: number): number {
  const numeric = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(numeric)) return fallback;
  return Math.max(min, Math.min(Math.trunc(numeric), max));
}

function deterministicEditText(params?: DeterministicEditParams): string {
  const text = typeof params?.text === 'string' ? params.text : 'edited-visible-proof';
  const normalized = text.trim();
  if (!normalized) {
    throw new Error('deterministic edit text is empty');
  }
  if (normalized.length > 128) {
    throw new Error('deterministic edit text is too long');
  }
  return normalized;
}

function resolveBodyParagraphTarget(
  sectionIndex: number,
  requestedParagraphIndex: unknown,
  fallbackParagraphIndex: number,
): BodyParagraphTarget {
  if (requestedParagraphIndex != null) {
    const paragraphIndex = boundedInteger(requestedParagraphIndex, fallbackParagraphIndex, 0, Number.MAX_SAFE_INTEGER);
    const paragraphLength = wasm.getParagraphLength(sectionIndex, paragraphIndex);
    if (paragraphLength <= 0) {
      throw new Error('internal paste proof requires a non-empty body paragraph');
    }
    return { paragraphIndex, paragraphLength };
  }

  const paragraphCount = wasm.getParagraphCount(sectionIndex);
  const fallbackIndex = boundedInteger(fallbackParagraphIndex, 0, 0, Math.max(0, paragraphCount - 1));
  for (let offset = 0; offset < paragraphCount; offset += 1) {
    const paragraphIndex = (fallbackIndex + offset) % paragraphCount;
    const paragraphLength = wasm.getParagraphLength(sectionIndex, paragraphIndex);
    if (paragraphLength > 0) {
      return { paragraphIndex, paragraphLength };
    }
  }

  throw new Error('internal paste proof requires a non-empty body paragraph');
}

function resolveBodyControlTarget(params?: ControlPasteCaptureProofParams): BodyControlTarget {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }

  if (params?.sectionIndex != null || params?.parentParaIndex != null || params?.controlIndex != null) {
    const sectionIndex = finiteIndex(params?.sectionIndex, 'sectionIndex');
    const parentParaIndex = finiteIndex(params?.parentParaIndex, 'parentParaIndex');
    const controlIndex = finiteIndex(params?.controlIndex, 'controlIndex');
    return {
      controlIndex,
      pageIndex: -1,
      parentParaIndex,
      sectionIndex,
      type: 'explicit',
    };
  }

  const seen = new Set<string>();
  for (let pageIndex = 0; pageIndex < wasm.pageCount; pageIndex += 1) {
    const layout = wasm.getPageControlLayout(pageIndex);
    const controls = Array.isArray(layout.controls)
      ? layout.controls as unknown as Array<Record<string, unknown>>
      : [];
    for (const control of controls) {
      if (control.type !== 'table') continue;

      const sectionIndex = Number(control.secIdx);
      const parentParaIndex = Number(control.paraIdx);
      const controlIndex = Number(control.controlIdx);
      if (![sectionIndex, parentParaIndex, controlIndex].every(Number.isInteger)) continue;

      const key = `${sectionIndex}:${parentParaIndex}:${controlIndex}`;
      if (seen.has(key)) continue;
      seen.add(key);

      return {
        controlIndex,
        pageIndex,
        parentParaIndex,
        sectionIndex,
        type: String(control.type),
      };
    }
  }

  throw new Error('control paste proof requires a body control target');
}

function applyDeterministicEdit(params?: DeterministicEditParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  const sectionCount = wasm.getSectionCount();
  const sectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const paragraphIndex = params?.paragraphIndex == null
    ? currentPosition.paragraphIndex
    : boundedInteger(params?.paragraphIndex, 0, 0, Number.MAX_SAFE_INTEGER);
  const charOffset = params?.charOffset == null
    ? currentPosition.charOffset
    : boundedInteger(params?.charOffset, 0, 0, Number.MAX_SAFE_INTEGER);
  const text = deterministicEditText(params);
  const position = {
    ...currentPosition,
    sectionIndex,
    paragraphIndex,
    charOffset,
  };

  inputHandler.executeOperation({
    kind: 'command',
    command: new InsertTextCommand(position, text),
    meta: {
      actionId: 'deterministic-edit-rpc',
      domain: 'text',
      refresh: 'full',
      dirtyScope: 'document',
      selection: 'moveToResult',
    },
  });

  return {
    changed: true,
    operation: 'insertText',
    runtimeIdentity,
    source: 'runtime_rpc',
    text,
    position: {
      sectionIndex,
      paragraphIndex,
      charOffset,
    },
  };
}

function runtimeProofHash(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `runtime-proof-${(hash >>> 0).toString(16).padStart(8, '0')}`;
}

function proofPngBytes(): Uint8Array {
  const binary = atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==');
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function runUndoRedoCaptureProof(params?: UndoRedoCaptureProofParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  inputHandler.resetCaptureCoverage();

  const sectionCount = wasm.getSectionCount();
  const sectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const paragraphIndex = params?.paragraphIndex == null
    ? currentPosition.paragraphIndex
    : boundedInteger(params?.paragraphIndex, 0, 0, Number.MAX_SAFE_INTEGER);
  const charOffset = params?.charOffset == null
    ? currentPosition.charOffset
    : boundedInteger(params?.charOffset, 0, 0, Number.MAX_SAFE_INTEGER);
  const text = deterministicEditText({ ...params, text: params?.text ?? 'undo-redo-proof' });
  const operationId = `runtime-undo-redo-${Date.now().toString(36)}`;
  const position = {
    ...currentPosition,
    sectionIndex,
    paragraphIndex,
    charOffset,
  };

  inputHandler.executeOperation({
    kind: 'command',
    command: new InsertTextCommand(position, text),
    meta: {
      actionId: 'undo-redo-capture-proof-rpc',
      domain: 'text',
      refresh: 'full',
      dirtyScope: 'document',
      selection: 'moveToResult',
    },
  });

  const canUndoAfterOperation = inputHandler.canUndo();
  if (!canUndoAfterOperation) {
    throw new Error('undo-redo proof could not observe undo availability after operation');
  }

  inputHandler.performUndo();
  const canRedoAfterUndo = inputHandler.canRedo();
  if (!canRedoAfterUndo) {
    throw new Error('undo-redo proof could not observe redo availability after undo');
  }

  inputHandler.performRedo();

  return {
    ok: true,
    operationId,
    text,
    position: {
      sectionIndex,
      paragraphIndex,
      charOffset,
    },
    events: [
      {
        eventId: `${operationId}-operation`,
        kind: 'operation',
        operation: {
          baseDocumentSha256: 'runtime-proof-base-unavailable',
          coverage: {
            unsupportedMutations: [],
            verdict: 'supported_candidate',
          },
          kind: 'body_text_replace',
          locator: {
            entryName: `Contents/section${sectionIndex}.xml`,
            expectedText: '',
          },
          manifestSchemaVersion: 1,
          operationId,
          payload: {
            payloadSha256: runtimeProofHash(text),
            replacementText: text,
          },
          runtimeIdentity,
          sourceHook: 'execute_operation_command',
        },
      },
      {
        eventId: `${operationId}-undo`,
        kind: 'undo',
        operationId,
      },
      {
        eventId: `${operationId}-redo`,
        kind: 'redo',
        operationId,
      },
    ],
    captureObservations: inputHandler.getCaptureCoverageObservations({
      categories: ['body_text_replace'],
    }),
    canUndoAfterOperation,
    canRedoAfterUndo,
    canUndoAfterRedo: inputHandler.canUndo(),
    canRedoAfterRedo: inputHandler.canRedo(),
    runtimeIdentity,
  };
}

function runRecordOnlyCaptureProof(params?: RecordOnlyCaptureProofParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  inputHandler.resetCaptureCoverage();

  const sectionCount = wasm.getSectionCount();
  const sectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const paragraphIndex = params?.paragraphIndex == null
    ? currentPosition.paragraphIndex
    : boundedInteger(params?.paragraphIndex, 0, 0, Number.MAX_SAFE_INTEGER);
  const charOffset = params?.charOffset == null
    ? currentPosition.charOffset
    : boundedInteger(params?.charOffset, 0, 0, Number.MAX_SAFE_INTEGER);
  const text = deterministicEditText({ ...params, text: params?.text ?? 'record-only-proof' });
  const operationId = `runtime-record-only-${Date.now().toString(36)}`;
  const position = {
    ...currentPosition,
    sectionIndex,
    paragraphIndex,
    charOffset,
  };
  const command = {
    type: 'insertText',
    timestamp: Date.now(),
    position,
    execute: () => position,
    undo: () => position,
    mergeWith: () => null,
  } satisfies EditCommand & { position: typeof position };

  inputHandler.executeOperation({
    kind: 'record',
    command,
    meta: {
      actionId: 'record-only-capture-proof-rpc',
      domain: 'text',
      refresh: 'none',
      dirtyScope: 'none',
      selection: 'none',
    },
  });

  return {
    ok: true,
    operationId,
    text,
    position: {
      sectionIndex,
      paragraphIndex,
      charOffset,
    },
    events: [
      {
        eventId: `${operationId}-record-only`,
        kind: 'record_only',
        reason: 'runtime command_history_record event is already reflected in preview and must not be replayed',
      },
    ],
    captureObservations: inputHandler.getCaptureCoverageObservations({
      categories: ['body_text_replace'],
    }),
    canUndoAfterRecord: inputHandler.canUndo(),
    canRedoAfterRecord: inputHandler.canRedo(),
    runtimeIdentity,
  };
}

function runImeCompositionCaptureProof(params?: ImeCompositionCaptureProofParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  inputHandler.resetCaptureCoverage();

  const sectionCount = wasm.getSectionCount();
  const sectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const paragraphIndex = params?.paragraphIndex == null
    ? currentPosition.paragraphIndex
    : boundedInteger(params?.paragraphIndex, 0, 0, Number.MAX_SAFE_INTEGER);
  const charOffset = params?.charOffset == null
    ? currentPosition.charOffset
    : boundedInteger(params?.charOffset, 0, 0, Number.MAX_SAFE_INTEGER);
  const text = deterministicEditText({ ...params, text: params?.text ?? 'ime-composition-proof' });
  const operationId = `runtime-ime-composition-${Date.now().toString(36)}`;
  const position = {
    ...currentPosition,
    sectionIndex,
    paragraphIndex,
    charOffset,
  };
  if (!inputHandler.moveCursorTo(position)) {
    throw new Error('IME composition proof target position is unavailable');
  }
  const proof = inputHandler.runImeCompositionCaptureProof(text);

  return {
    ok: true,
    operationId,
    text,
    position: {
      sectionIndex,
      paragraphIndex,
      charOffset,
    },
    afterPosition: {
      sectionIndex: proof.afterPosition.sectionIndex,
      paragraphIndex: proof.afterPosition.paragraphIndex,
      charOffset: proof.afterPosition.charOffset,
    },
    insertedText: proof.insertedText,
    events: [
      {
        eventId: `${operationId}-operation`,
        kind: 'operation',
        operation: {
          baseDocumentSha256: 'runtime-proof-base-unavailable',
          coverage: {
            unsupportedMutations: [],
            verdict: 'supported_candidate',
          },
          kind: 'body_text_replace',
          locator: {
            entryName: `Contents/section${sectionIndex}.xml`,
            expectedText: '',
          },
          manifestSchemaVersion: 1,
          operationId,
          payload: {
            payloadSha256: runtimeProofHash(proof.insertedText),
            replacementText: proof.insertedText,
          },
          runtimeIdentity,
          sourceHook: 'command_history_record',
        },
      },
      {
        eventId: `${operationId}-record-only`,
        kind: 'record_only',
        reason: 'IME composition raw mutation is already reflected in preview before command_history_record',
      },
    ],
    captureObservations: inputHandler.getCaptureCoverageObservations({
      categories: ['body_text_replace'],
    }),
    canUndoAfterComposition: inputHandler.canUndo(),
    canRedoAfterComposition: inputHandler.canRedo(),
    runtimeIdentity,
  };
}

function iosFallbackIntermediateText(params: IosFallbackInputCaptureProofParams | undefined, finalText: string): string {
  const text = typeof params?.intermediateText === 'string'
    ? params.intermediateText
    : finalText.slice(0, Math.max(1, Math.floor(finalText.length / 2)));
  const normalized = text.trim();
  if (!normalized) {
    throw new Error('iOS fallback intermediate text is empty');
  }
  if (normalized.length > 128) {
    throw new Error('iOS fallback intermediate text is too long');
  }
  if (normalized === finalText) {
    throw new Error('iOS fallback intermediate text must differ from final text');
  }
  return normalized;
}

function runIosFallbackInputCaptureProof(params?: IosFallbackInputCaptureProofParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  inputHandler.resetCaptureCoverage();

  const sectionCount = wasm.getSectionCount();
  const sectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const paragraphIndex = params?.paragraphIndex == null
    ? currentPosition.paragraphIndex
    : boundedInteger(params?.paragraphIndex, 0, 0, Number.MAX_SAFE_INTEGER);
  const charOffset = params?.charOffset == null
    ? currentPosition.charOffset
    : boundedInteger(params?.charOffset, 0, 0, Number.MAX_SAFE_INTEGER);
  const text = deterministicEditText({ ...params, text: params?.text ?? 'ios-fallback-proof' });
  const intermediateText = iosFallbackIntermediateText(params, text);
  const operationId = `runtime-ios-fallback-${Date.now().toString(36)}`;
  const position = {
    ...currentPosition,
    sectionIndex,
    paragraphIndex,
    charOffset,
  };
  if (!inputHandler.moveCursorTo(position)) {
    throw new Error('iOS fallback proof target position is unavailable');
  }
  const proof = inputHandler.runIosFallbackInputCaptureProof(text, intermediateText);

  return {
    ok: true,
    operationId,
    text,
    intermediateText: proof.intermediateText,
    position: {
      sectionIndex,
      paragraphIndex,
      charOffset,
    },
    afterPosition: {
      sectionIndex: proof.afterPosition.sectionIndex,
      paragraphIndex: proof.afterPosition.paragraphIndex,
      charOffset: proof.afterPosition.charOffset,
    },
    insertedText: proof.insertedText,
    events: [
      {
        eventId: `${operationId}-intermediate-raw`,
        kind: 'snapshot',
        reason: 'iOS fallback intermediate raw mutation is replaced by the final input value',
      },
      {
        eventId: `${operationId}-operation`,
        kind: 'operation',
        operation: {
          baseDocumentSha256: 'runtime-proof-base-unavailable',
          coverage: {
            unsupportedMutations: [],
            verdict: 'supported_candidate',
          },
          kind: 'body_text_replace',
          locator: {
            entryName: `Contents/section${sectionIndex}.xml`,
            expectedText: '',
          },
          manifestSchemaVersion: 1,
          operationId,
          payload: {
            payloadSha256: runtimeProofHash(proof.insertedText),
            replacementText: proof.insertedText,
          },
          runtimeIdentity,
          sourceHook: 'wasm_insert_text_at_raw_ios_fallback',
        },
      },
    ],
    captureObservations: inputHandler.getCaptureCoverageObservations({
      categories: ['body_text_replace'],
    }),
    canUndoAfterFallback: inputHandler.canUndo(),
    canRedoAfterFallback: inputHandler.canRedo(),
    runtimeIdentity,
  };
}

function runImagePasteCaptureProof(params?: ImagePasteCaptureProofParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  inputHandler.resetCaptureCoverage();

  const sectionCount = wasm.getSectionCount();
  const sectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const paragraphIndex = params?.paragraphIndex == null
    ? currentPosition.paragraphIndex
    : boundedInteger(params?.paragraphIndex, 0, 0, Number.MAX_SAFE_INTEGER);
  const charOffset = params?.charOffset == null
    ? currentPosition.charOffset
    : boundedInteger(params?.charOffset, 0, 0, Number.MAX_SAFE_INTEGER);
  const operationId = `runtime-image-paste-${Date.now().toString(36)}`;
  const position = {
    ...currentPosition,
    sectionIndex,
    paragraphIndex,
    charOffset,
  };
  const imageData = proofPngBytes();
  let insertResult: { ok: boolean; paraIdx?: number; controlIdx?: number } = { ok: false };

  inputHandler.executeOperation({
    kind: 'snapshot',
    operationType: 'pasteImage',
    operation: (bridge) => {
      insertResult = bridge.insertPicture(
        sectionIndex,
        paragraphIndex,
        charOffset,
        '',
        imageData,
        75,
        75,
        1,
        1,
        'png',
        'runtime image paste capture proof',
      );
      if (insertResult.ok) {
        return {
          sectionIndex,
          paragraphIndex: (insertResult.paraIdx ?? paragraphIndex) + 1,
          charOffset: 0,
        };
      }
      return position;
    },
    meta: {
      actionId: 'image-paste-capture-proof-rpc',
      domain: 'unknown',
      refresh: 'full',
      dirtyScope: 'document',
    },
  });

  return {
    ok: insertResult.ok === true,
    operation: 'pasteImage',
    operationId,
    position: {
      sectionIndex,
      paragraphIndex,
      charOffset,
    },
    target: {
      paraIdx: insertResult.paraIdx,
      controlIdx: insertResult.controlIdx,
    },
    events: [
      {
        eventId: `${operationId}-unsupported`,
        kind: 'unsupported',
        mutation: 'pasteImage',
        sourceHook: 'snapshot_pasteImage',
      },
    ],
    captureObservations: inputHandler.getCaptureCoverageObservations({
      categories: ['complex_paste'],
    }),
    runtimeIdentity,
  };
}

function runControlPasteCaptureProof(params?: ControlPasteCaptureProofParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  inputHandler.resetCaptureCoverage();

  const sectionCount = wasm.getSectionCount();
  const requestedSectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const controlTarget = resolveBodyControlTarget(params);
  const pasteSectionIndex = params?.sectionIndex == null ? controlTarget.sectionIndex : requestedSectionIndex;
  const { paragraphIndex, paragraphLength } = resolveBodyParagraphTarget(
    pasteSectionIndex,
    params?.paragraphIndex,
    currentPosition.paragraphIndex,
  );
  const pasteOffset = params?.charOffset == null
    ? paragraphLength
    : boundedInteger(params?.charOffset, paragraphLength, 0, paragraphLength);
  const operationId = `runtime-control-paste-${Date.now().toString(36)}`;
  const position = {
    ...currentPosition,
    sectionIndex: pasteSectionIndex,
    paragraphIndex,
    charOffset: pasteOffset,
  };
  const copiedText = wasm.copyControl(
    controlTarget.sectionIndex,
    controlTarget.parentParaIndex,
    controlTarget.controlIndex,
  );
  if (!copiedText || !wasm.hasInternalClipboard() || !wasm.clipboardHasControl()) {
    throw new Error('control paste proof could not prepare control clipboard');
  }

  let pasteResult: { ok: boolean; paraIdx?: number; controlIdx?: number } = { ok: false };

  inputHandler.executeOperation({
    kind: 'snapshot',
    operationType: 'pasteControl',
    operation: (bridge) => {
      const result = bridge.pasteControl(pasteSectionIndex, paragraphIndex, pasteOffset);
      pasteResult = JSON.parse(result);
      if (pasteResult.ok) {
        return {
          sectionIndex: pasteSectionIndex,
          paragraphIndex: (pasteResult.paraIdx ?? paragraphIndex) + 1,
          charOffset: 0,
        };
      }
      return position;
    },
    meta: {
      actionId: 'control-paste-capture-proof-rpc',
      domain: 'unknown',
      refresh: 'full',
      dirtyScope: 'document',
    },
  });

  return {
    ok: pasteResult.ok === true,
    operation: 'pasteControl',
    operationId,
    copiedText,
    source: controlTarget,
    position: {
      sectionIndex: pasteSectionIndex,
      paragraphIndex,
      charOffset: pasteOffset,
    },
    target: {
      paraIdx: pasteResult.paraIdx,
      controlIdx: pasteResult.controlIdx,
    },
    events: [
      {
        eventId: `${operationId}-unsupported`,
        kind: 'unsupported',
        mutation: 'pasteControl',
        sourceHook: 'snapshot_pasteControl',
      },
    ],
    captureObservations: inputHandler.getCaptureCoverageObservations({
      categories: ['complex_paste'],
    }),
    runtimeIdentity,
  };
}

function runInternalPasteCaptureProof(params?: InternalPasteCaptureProofParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  inputHandler.resetCaptureCoverage();

  const sectionCount = wasm.getSectionCount();
  const sectionIndex = boundedInteger(params?.sectionIndex, 0, 0, Math.max(0, sectionCount - 1));
  const currentPosition = inputHandler.getCursorPosition();
  const { paragraphIndex, paragraphLength } = resolveBodyParagraphTarget(
    sectionIndex,
    params?.paragraphIndex,
    currentPosition.paragraphIndex,
  );

  const requestedCopyLength = params?.copyLength == null
    ? 4
    : boundedInteger(params.copyLength, 4, 1, 64);
  const copyLength = Math.max(1, Math.min(requestedCopyLength, paragraphLength));
  const pasteOffset = params?.charOffset == null
    ? paragraphLength
    : boundedInteger(params?.charOffset, paragraphLength, 0, paragraphLength);
  const operationId = `runtime-internal-paste-${Date.now().toString(36)}`;
  const position = {
    ...currentPosition,
    sectionIndex,
    paragraphIndex,
    charOffset: pasteOffset,
  };
  const copyResult = JSON.parse(wasm.copySelection(sectionIndex, paragraphIndex, 0, paragraphIndex, copyLength));
  if (!copyResult.ok || !wasm.hasInternalClipboard()) {
    throw new Error('internal paste proof could not prepare internal clipboard');
  }

  let pasteResult: { ok: boolean; paraIdx?: number; charOffset?: number } = { ok: false };

  inputHandler.executeOperation({
    kind: 'snapshot',
    operationType: 'pasteInternal',
    operation: (bridge) => {
      const result = bridge.pasteInternal(sectionIndex, paragraphIndex, pasteOffset);
      pasteResult = JSON.parse(result);
      if (pasteResult.ok) {
        return {
          sectionIndex,
          paragraphIndex: pasteResult.paraIdx ?? paragraphIndex,
          charOffset: pasteResult.charOffset ?? pasteOffset,
        };
      }
      return position;
    },
    meta: {
      actionId: 'internal-paste-capture-proof-rpc',
      domain: 'unknown',
      refresh: 'full',
      dirtyScope: 'document',
    },
  });

  return {
    ok: pasteResult.ok === true,
    operation: 'pasteInternal',
    operationId,
    copiedText: copyResult.text,
    position: {
      sectionIndex,
      paragraphIndex,
      charOffset: pasteOffset,
    },
    target: {
      paraIdx: pasteResult.paraIdx,
      charOffset: pasteResult.charOffset,
    },
    events: [
      {
        eventId: `${operationId}-unsupported`,
        kind: 'unsupported',
        mutation: 'pasteInternal',
        sourceHook: 'snapshot_pasteInternal',
      },
    ],
    captureObservations: inputHandler.getCaptureCoverageObservations({
      categories: ['complex_paste'],
    }),
    runtimeIdentity,
  };
}

function tableCellText(params?: TableCellTextParams): string {
  const text = typeof params?.text === 'string' ? params.text : 'table-cell-proof';
  if (text.length === 0) {
    throw new Error('table cell replacement text is empty');
  }
  if (text.length > 128) {
    throw new Error('table cell replacement text is too long');
  }
  return text;
}

function footnoteProofText(params?: FootnoteTextParams): string {
  const text = typeof params?.text === 'string' ? params.text : ' footnote-proof';
  if (text.length === 0) {
    throw new Error('footnote proof text is empty');
  }
  if (text.length > 128) {
    throw new Error('footnote proof text is too long');
  }
  return text;
}

function finiteIndex(value: unknown, name: string): number {
  const numeric = Number(value);
  if (!Number.isInteger(numeric) || numeric < 0) {
    throw new Error(`${name} must be a non-negative integer.`);
  }
  return numeric;
}

function getFootnoteTextTargets(): FootnoteTextTarget[] {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }

  const targets: FootnoteTextTarget[] = [];
  const seen = new Set<string>();

  for (let pageIndex = 0; pageIndex < wasm.pageCount; pageIndex += 1) {
    for (let footnoteIndex = 0; footnoteIndex < 256; footnoteIndex += 1) {
      let pageFootnote;
      try {
        pageFootnote = wasm.getPageFootnoteInfo(pageIndex, footnoteIndex);
      } catch {
        break;
      }
      if (!pageFootnote?.ok) break;

      const sectionIndex = pageFootnote.sectionIdx;
      const parentParaIndex = pageFootnote.paraIdx;
      const controlIndex = pageFootnote.controlIdx;
      let info;
      try {
        info = wasm.getFootnoteInfo(sectionIndex, parentParaIndex, controlIndex);
      } catch {
        continue;
      }
      if (!info.ok) continue;

      for (let fnParaIndex = 0; fnParaIndex < info.texts.length; fnParaIndex += 1) {
        const value = info.texts[fnParaIndex] ?? '';
        const key = `${sectionIndex}:${parentParaIndex}:${controlIndex}:${fnParaIndex}`;
        if (seen.has(key)) continue;
        seen.add(key);

        targets.push({
          charLength: value.length,
          controlIndex,
          footnoteIndex,
          footnoteNumber: info.number,
          fnParaIndex,
          pageIndex,
          parentParaIndex,
          sectionIndex,
          sourceType: pageFootnote.sourceType,
          value,
        });
      }
    }
  }

  return targets;
}

function getTableCellTextTargets(): TableCellTextTarget[] {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }

  const targets: TableCellTextTarget[] = [];
  const seen = new Set<string>();

  for (let pageIndex = 0; pageIndex < wasm.pageCount; pageIndex += 1) {
    const layout = wasm.getPageControlLayout(pageIndex);
    const controls = Array.isArray(layout.controls)
      ? layout.controls as unknown as Array<Record<string, unknown>>
      : [];
    for (const control of controls) {
      if (control.type !== 'table') continue;

      const sectionIndex = Number(control.secIdx);
      const parentParaIndex = Number(control.paraIdx);
      const controlIndex = Number(control.controlIdx);
      if (![sectionIndex, parentParaIndex, controlIndex].every(Number.isInteger)) continue;

      const cells = Array.isArray(control.cells) ? control.cells as Array<Record<string, unknown>> : [];
      for (const cell of cells) {
        const cellIndex = Number(cell.cellIdx);
        if (!Number.isInteger(cellIndex)) continue;

        const cellParaIndex = 0;
        const key = `${sectionIndex}:${parentParaIndex}:${controlIndex}:${cellIndex}:${cellParaIndex}`;
        if (seen.has(key)) continue;
        seen.add(key);

        let paragraphCount = 0;
        let charLength = 0;
        let value = '';
        try {
          paragraphCount = wasm.getCellParagraphCount(sectionIndex, parentParaIndex, controlIndex, cellIndex);
          if (paragraphCount <= 0) continue;
          charLength = wasm.getCellParagraphLength(sectionIndex, parentParaIndex, controlIndex, cellIndex, cellParaIndex);
          value = charLength > 0
            ? wasm.getTextInCell(sectionIndex, parentParaIndex, controlIndex, cellIndex, cellParaIndex, 0, charLength)
            : '';
        } catch {
          continue;
        }

        targets.push({
          cellIndex,
          cellParaIndex,
          charLength,
          col: Number.isInteger(Number(cell.col)) ? Number(cell.col) : undefined,
          controlIndex,
          pageIndex,
          parentParaIndex,
          row: Number.isInteger(Number(cell.row)) ? Number(cell.row) : undefined,
          sectionIndex,
          value,
        });
      }
    }
  }

  return targets;
}

function setTableCellText(params?: TableCellTextParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  const sectionIndex = finiteIndex(params?.sectionIndex, 'sectionIndex');
  const parentParaIndex = finiteIndex(params?.parentParaIndex, 'parentParaIndex');
  const controlIndex = finiteIndex(params?.controlIndex, 'controlIndex');
  const cellIndex = finiteIndex(params?.cellIndex, 'cellIndex');
  const cellParaIndex = params?.cellParaIndex == null ? 0 : finiteIndex(params.cellParaIndex, 'cellParaIndex');
  const text = tableCellText(params);
  const oldLength = wasm.getCellParagraphLength(sectionIndex, parentParaIndex, controlIndex, cellIndex, cellParaIndex);
  const oldValue = oldLength > 0
    ? wasm.getTextInCell(sectionIndex, parentParaIndex, controlIndex, cellIndex, cellParaIndex, 0, oldLength)
    : '';
  const position = {
    sectionIndex,
    paragraphIndex: parentParaIndex,
    charOffset: 0,
    parentParaIndex,
    controlIndex,
    cellIndex,
    cellParaIndex,
  };

  if (oldLength > 0) {
    inputHandler.executeOperation({
      kind: 'command',
      command: new DeleteTextCommand(position, oldLength, 'forward'),
      meta: {
        actionId: 'table-cell-text-rpc-delete',
        domain: 'table',
        refresh: 'full',
        dirtyScope: 'table',
        selection: 'moveToResult',
      },
    });
  }

  inputHandler.executeOperation({
    kind: 'command',
    command: new InsertTextCommand(position, text),
    meta: {
      actionId: 'table-cell-text-rpc-insert',
      domain: 'table',
      refresh: 'full',
      dirtyScope: 'table',
      selection: 'moveToResult',
    },
  });

  return {
    ok: true,
    oldLength,
    oldValue,
    newValue: text,
    target: {
      sectionIndex,
      parentParaIndex,
      controlIndex,
      cellIndex,
      cellParaIndex,
    },
  };
}

function setFootnoteTextForProof(params?: FootnoteTextParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  const sectionIndex = finiteIndex(params?.sectionIndex, 'sectionIndex');
  const parentParaIndex = finiteIndex(params?.parentParaIndex, 'parentParaIndex');
  const controlIndex = finiteIndex(params?.controlIndex, 'controlIndex');
  const fnParaIndex = params?.fnParaIndex == null ? 0 : finiteIndex(params.fnParaIndex, 'fnParaIndex');
  const text = footnoteProofText(params);
  const info = wasm.getFootnoteInfo(sectionIndex, parentParaIndex, controlIndex);
  if (!info.ok) {
    throw new Error('footnote target is unavailable');
  }
  const oldValue = info.texts[fnParaIndex] ?? '';
  const charOffset = oldValue.length;
  const result = wasm.insertTextInFootnote(
    sectionIndex,
    parentParaIndex,
    controlIndex,
    fnParaIndex,
    charOffset,
    text,
  );

  if (result.ok === true) {
    inputHandler.commitExternalDirectMutation('footnote_text_replace', 'wasm_insert_text_in_footnote');
  }

  return {
    ok: result.ok === true,
    operation: 'insertTextInFootnote',
    oldValue,
    newValue: `${oldValue}${text}`,
    target: {
      sectionIndex,
      parentParaIndex,
      controlIndex,
      fnParaIndex,
      charOffset,
    },
  };
}

function finiteDelta(value: unknown, name: string): number {
  const numeric = Number(value);
  if (!Number.isFinite(numeric) || numeric === 0) {
    throw new Error(`${name} must be a non-zero finite number.`);
  }
  if (Math.abs(numeric) > 2_000) {
    throw new Error(`${name} is too large for proof resize.`);
  }
  return Math.trunc(numeric);
}

function resizeTableCellForProof(params?: TableCellResizeParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  const sectionIndex = finiteIndex(params?.sectionIndex, 'sectionIndex');
  const parentParaIndex = finiteIndex(params?.parentParaIndex, 'parentParaIndex');
  const controlIndex = finiteIndex(params?.controlIndex, 'controlIndex');
  const cellIndex = finiteIndex(params?.cellIndex, 'cellIndex');
  const widthDelta = params?.widthDelta == null ? 200 : finiteDelta(params.widthDelta, 'widthDelta');
  const heightDelta = params?.heightDelta == null ? undefined : finiteDelta(params.heightDelta, 'heightDelta');
  const updates = [{
    cellIdx: cellIndex,
    widthDelta,
    ...(heightDelta == null ? {} : { heightDelta }),
  }];
  const position = inputHandler.getCursorPosition();

  inputHandler.executeOperation({
    kind: 'snapshot',
    operationType: 'resizeTableCells',
    operation: (bridge) => {
      bridge.resizeTableCells(sectionIndex, parentParaIndex, controlIndex, updates);
      return position;
    },
    meta: {
      actionId: 'table-cell-resize-proof-rpc',
      domain: 'table',
      refresh: 'full',
      dirtyScope: 'table',
    },
  });

  return {
    ok: true,
    operation: 'resizeTableCells',
    target: {
      sectionIndex,
      parentParaIndex,
      controlIndex,
      cellIndex,
    },
    updates,
  };
}

function parseClickHereFieldId(params?: ClickHerePropsParams): number {
  const fieldId = Number(params?.fieldId);
  if (!Number.isInteger(fieldId)) {
    throw new Error('Field id is required.');
  }

  return fieldId;
}

function getClickHereProps(params?: ClickHerePropsParams): Record<string, unknown> {
  return wasm.getClickHereProps(parseClickHereFieldId(params));
}

function updateClickHerePropsForProof(params?: ClickHerePropsParams): Record<string, unknown> {
  const fieldId = parseClickHereFieldId(params);
  const guide = String(params?.guide ?? '');
  const memo = String(params?.memo ?? '');
  const name = String(params?.name ?? '');
  const editable = Boolean(params?.editable ?? true);
  const result = wasm.updateClickHereProps(fieldId, guide, memo, name, editable);

  if (result.ok === true) {
    inputHandler?.commitExternalUnsupportedDirectMutation(
      'field_metadata_mutation',
      'wasm_update_click_here_props',
      'fieldMetadataUpdate',
    );
  }

  return result;
}

function recordFieldMetadataMutationForProof(params?: ClickHerePropsParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }
  if (!inputHandler) {
    throw new Error('input handler is unavailable');
  }

  const fieldId = parseClickHereFieldId(params);
  const fields = wasm.getFieldList();
  const target = fields.find((field) => field.fieldId === fieldId);
  if (!target) {
    throw new Error(`Field id ${fieldId} is unavailable.`);
  }

  inputHandler.recordExternalUnsupportedDirectMutation(
    'field_metadata_mutation',
    'wasm_field_metadata_mutation_proof',
    'fieldMetadataUpdate',
  );

  return {
    ok: true,
    fieldId,
    fieldType: target.fieldType,
    mutation: 'fieldMetadataUpdate',
  };
}

function removeFieldByIdForProof(params?: ClickHerePropsParams): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }

  const fieldId = parseClickHereFieldId(params);
  const result = wasm.removeFieldById(fieldId);

  if (result.ok === true) {
    inputHandler?.commitExternalUnsupportedDirectMutation(
      'field_metadata_mutation',
      'wasm_remove_field_by_id',
      'fieldDelete',
    );
  }

  return {
    ...result,
    mutation: 'fieldDelete',
  };
}

function insertHeaderFooterFieldForProof(params?: Record<string, unknown>): Record<string, unknown> {
  if (!wasm.hasLoadedDocument()) {
    throw new Error('문서가 로드되지 않았습니다');
  }

  const sectionCount = wasm.getSectionCount();
  const requestedSectionIndex = Number(params?.sectionIndex);
  const requestedIsHeader = typeof params?.isHeader === 'boolean' ? params.isHeader : undefined;
  const requestedApplyTo = Number(params?.applyTo);
  const requestedParaIndex = Number(params?.hfParaIndex);
  const requestedCharOffset = Number(params?.charOffset);
  const fieldType = Number.isInteger(Number(params?.fieldType)) ? Number(params?.fieldType) : 1;
  const sectionIndexes = Number.isInteger(requestedSectionIndex)
    ? [requestedSectionIndex]
    : Array.from({ length: sectionCount }, (_, index) => index);
  const headerFlags = requestedIsHeader === undefined ? [true, false] : [requestedIsHeader];
  const applyToValues = Number.isInteger(requestedApplyTo) ? [requestedApplyTo] : [0, 1, 2, 3];

  for (const sectionIndex of sectionIndexes) {
    for (const isHeader of headerFlags) {
      for (const applyTo of applyToValues) {
        const summary = JSON.parse(wasm.getHeaderFooter(sectionIndex, isHeader, applyTo)) as Record<string, unknown>;
        if (!summary.exists) continue;

        const text = typeof summary.text === 'string' ? summary.text : '';
        const hfParaIndex = Number.isInteger(requestedParaIndex) ? requestedParaIndex : 0;
        const charOffset = Number.isInteger(requestedCharOffset) ? requestedCharOffset : text.length;
        const result = wasm.insertFieldInHf(sectionIndex, isHeader, applyTo, hfParaIndex, charOffset, fieldType);

        if (result.ok === true) {
          inputHandler?.commitExternalUnsupportedDirectMutation(
            'field_metadata_mutation',
            'wasm_insert_field_in_hf',
            'fieldCreate',
          );
        }

        return {
          ...result,
          mutation: 'fieldCreate',
          target: {
            applyTo,
            charOffset,
            fieldType,
            hfParaIndex,
            isHeader,
            sectionIndex,
          },
        };
      }
    }
  }

  return {
    error: 'No header/footer target is available.',
    mutation: 'fieldCreate',
    ok: false,
  };
}

// E2E 테스트용 전역 노출 (개발 모드 전용)
if (import.meta.env.DEV) {
  (window as any).__wasm = wasm;
  (window as any).__eventBus = eventBus;
  (window as any).__documentState = documentState;
  initRhwpDev(wasm);
}
let canvasView: CanvasView | null = null;
let inputHandler: InputHandler | null = null;
let toolbar: Toolbar | null = null;
let ruler: Ruler | null = null;


// ─── 커맨드 시스템 ─────────────────────────────
const registry = new CommandRegistry();

function getContext(): EditorContext {
  const hasDoc = wasm.pageCount > 0;
  return {
    hasDocument: hasDoc,
    hasSelection: inputHandler?.hasSelection() ?? false,
    inTable: inputHandler?.isInTable() ?? false,
    inCellSelectionMode: inputHandler?.isInCellSelectionMode() ?? false,
    inTableObjectSelection: inputHandler?.isInTableObjectSelection() ?? false,
    inPictureObjectSelection: inputHandler?.isInPictureObjectSelection() ?? false,
    inField: inputHandler?.isInField() ?? false,
    isEditable: true,
    canUndo: inputHandler?.canUndo() ?? false,
    canRedo: inputHandler?.canRedo() ?? false,
    zoom: canvasView?.getViewportManager().getZoom() ?? 1.0,
    showControlCodes: wasm.getShowControlCodes(),
    isDirty: documentState.isDirty(),
    sourceFormat: hasDoc ? (wasm.getSourceFormat() as 'hwp' | 'hwpx') : undefined,
  };
}

const commandServices: CommandServices = {
  eventBus,
  wasm,
  documentState,
  getContext,
  getInputHandler: () => inputHandler,
  getViewportManager: () => canvasView?.getViewportManager() ?? null,
};

const dispatcher = new CommandDispatcher(registry, commandServices, eventBus);

// 모든 내장 커맨드 등록
registry.registerAll(fileCommands);
registry.registerAll(editCommands);
registry.registerAll(viewCommands);
registry.registerAll(formatCommands);
registry.registerAll(insertCommands);
registry.registerAll(tableCommands);
registry.registerAll(pageCommands);
registry.registerAll(toolCommands);

// 상태 바 요소
const sbMessage = () => document.getElementById('sb-message')!;
const sbPage = () => document.getElementById('sb-page')!;
const sbSection = () => document.getElementById('sb-section')!;
const sbZoomVal = () => document.getElementById('sb-zoom-val')!;

async function initialize(): Promise<void> {
  const msg = sbMessage();
  try {
    msg.textContent = '웹폰트 로딩 중...';
    await loadWebFonts([]);  // CSS @font-face 등록 + CRITICAL 폰트만 로드
    msg.textContent = 'WASM 로딩 중...';
    await wasm.initialize();
    if (import.meta.env.DEV) {
      initRhwpDev(wasm);
    }
    const renderBackendRequest = resolveRenderBackendRequest(window.location.search);
    const canvaskitMode = resolveCanvasKitRenderMode(window.location.search);
    const canvaskitSurfaceRequest = resolveCanvasKitSurfaceRequest(window.location.search);
    const renderProfile = resolveRenderProfile(window.location.search);
    if (renderBackendRequest.unsupportedReason) {
      console.warn(
        `[main] 지원하지 않는 renderer 값입니다: ${renderBackendRequest.requested}; Canvas2D를 사용합니다.`,
      );
    }
    let renderBackend = renderBackendRequest.backend;
    let canvaskitRenderer: CanvasKitLayerRenderer | null = null;

    if (renderBackend === 'canvaskit') {
      msg.textContent = 'CanvasKit 로딩 중...';
      try {
        const { CanvasKitLayerRenderer } = await import('@/view/canvaskit-renderer');
        canvaskitRenderer = await CanvasKitLayerRenderer.create(canvaskitMode, canvaskitSurfaceRequest);
      } catch (error) {
        console.error('[main] CanvasKit 초기화 실패, Canvas2D로 폴백합니다:', error);
        renderBackend = 'canvas2d';
      }
    }
    msg.textContent = 'HWP 파일을 선택해주세요.';

    const container = document.getElementById('scroll-container')!;
    canvasView = new CanvasView(
      container,
      wasm,
      eventBus,
      renderBackend,
      renderProfile,
      canvaskitRenderer,
    );

    // 눈금자 초기화
    ruler = new Ruler(
      document.getElementById('h-ruler') as HTMLCanvasElement,
      document.getElementById('v-ruler') as HTMLCanvasElement,
      container,
      eventBus,
      wasm,
      canvasView.getVirtualScroll(),
      canvasView.getViewportManager(),
    );

    inputHandler = new InputHandler(
      container, wasm, eventBus,
      canvasView.getVirtualScroll(),
      canvasView.getViewportManager(),
    );

    toolbar = new Toolbar(document.getElementById('style-bar')!, wasm, eventBus, dispatcher);
    toolbar.setEnabled(false);

    // InputHandler에 커맨드 디스패처 및 컨텍스트 메뉴 주입
    inputHandler.setDispatcher(dispatcher);
    inputHandler.setContextMenu(new ContextMenu(dispatcher, registry));
    inputHandler.setCommandPalette(new CommandPalette(registry, dispatcher));
    inputHandler.setCellSelectionRenderer(
      new CellSelectionRenderer(container, canvasView.getVirtualScroll()),
    );
    inputHandler.setTableObjectRenderer(
      new TableObjectRenderer(container, canvasView.getVirtualScroll()),
    );
    inputHandler.setTableResizeRenderer(
      new TableResizeRenderer(container, canvasView.getVirtualScroll()),
    );
    inputHandler.setPictureObjectRenderer(
      new TableObjectRenderer(container, canvasView.getVirtualScroll(), true),
    );

    new MenuBar(document.getElementById('menu-bar')!, eventBus, dispatcher);

    // 툴바 내 data-cmd 버튼 클릭 → 커맨드 디스패치
    document.querySelectorAll('.tb-btn[data-cmd]').forEach(btn => {
      btn.addEventListener('mousedown', (e) => {
        e.preventDefault();
        const cmd = (btn as HTMLElement).dataset.cmd;
        if (cmd) dispatcher.dispatch(cmd, { anchorEl: btn as HTMLElement });
      });
    });

    // 스플릿 버튼 드롭다운 메뉴
    document.querySelectorAll('.tb-split').forEach(split => {
      const arrow = split.querySelector('.tb-split-arrow');
      if (arrow) {
        arrow.addEventListener('mousedown', (e) => {
          e.preventDefault();
          e.stopPropagation();
          // 다른 열린 메뉴 닫기
          document.querySelectorAll('.tb-split.open').forEach(s => {
            if (s !== split) s.classList.remove('open');
          });
          split.classList.toggle('open');
        });
      }
      split.querySelectorAll('.tb-split-item[data-cmd]').forEach(item => {
        item.addEventListener('mousedown', (e) => {
          e.preventDefault();
          split.classList.remove('open');
          const cmd = (item as HTMLElement).dataset.cmd;
          if (cmd) dispatcher.dispatch(cmd, { anchorEl: item as HTMLElement });
        });
      });
    });
    // 외부 클릭 시 스플릿 메뉴 닫기
    document.addEventListener('mousedown', () => {
      document.querySelectorAll('.tb-split.open').forEach(s => s.classList.remove('open'));
    });

    // #780: 도구 모음/서식 도구 모음 영역 mousedown 시 focus 이동 방지
    // — 편집 영역의 텍스트 선택(cursor.anchor)이 보존되어야 서식 적용이 동작함
    for (const id of ['icon-toolbar', 'style-bar']) {
      const el = document.getElementById(id);
      if (el) el.addEventListener('mousedown', (e) => {
        if ((e.target as HTMLElement).tagName !== 'INPUT' && (e.target as HTMLElement).tagName !== 'SELECT') {
          e.preventDefault();
        }
      });
    }

    setupFileInput();
    setupZoomControls();
    setupEventListeners();
    setupGlobalShortcuts();
    loadFromUrlParam();
    installPwaFileHandling(window as FileHandlingWindowLike, {
      openDocumentBytes(payload) {
        eventBus.emit('open-document-bytes', payload);
      },
      notifyUnsupportedFile(fileName) {
        showLoadError(new Error(`지원하지 않는 파일 형식입니다: ${fileName}. HWP/HWPX 파일만 지원합니다.`));
      },
      notifyError(error) {
        showLoadError(error);
      },
      notifyMultipleFiles(count) {
        console.warn(`[pwa-file-handling] 여러 파일(${count}개)이 전달되어 첫 번째 파일만 엽니다.`);
      },
    });

    // E2E 테스트용 전역 노출 (개발 모드 전용)
    if (import.meta.env.DEV) {
      (window as any).__inputHandler = inputHandler;
      (window as any).__canvasView = canvasView;
      (window as any).__renderBackend = renderBackend;
      (window as any).__canvaskitRenderMode = canvaskitMode;
      (window as any).__canvaskitSurfaceRequest = canvaskitSurfaceRequest;
      (window as any).__renderProfile = renderProfile;
    }
  } catch (error) {
    msg.textContent = `WASM 초기화 실패: ${error}`;
    console.error('[main] WASM 초기화 실패:', error);
  }
}

/**
 * 전역 단축키 핸들러 — InputHandler.active 여부와 무관하게 동작해야 하는 단축키.
 * 예: 문서 미로드 상태에서도 Alt+N(새 문서), Ctrl+O(열기) 등.
 */
function setupGlobalShortcuts(): void {
  document.addEventListener('keydown', (e) => {
    // input/textarea 등 편집 가능 요소 내부에서는 무시
    const target = e.target as HTMLElement;
    if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) return;
    // InputHandler가 활성 상태이면 자체 처리에 맡김
    if (inputHandler?.isActive()) return;

    const ctrlOrMeta = e.ctrlKey || e.metaKey;

    // Alt+N / Alt+ㅜ → 새 문서 (문서 미로드 상태에서도 동작)
    if (e.altKey && !ctrlOrMeta && !e.shiftKey) {
      if (e.key === 'n' || e.key === 'N' || e.key === 'ㅜ') {
        e.preventDefault();
        dispatcher.dispatch('file:new-doc');
        return;
      }
    }
    // Ctrl/Cmd+O → 열기 (문서 미로드 상태에서도 동작)
    if (ctrlOrMeta && !e.altKey && !e.shiftKey) {
      if (e.key === 'o' || e.key === 'O' || e.key === 'ㅐ') {
        e.preventDefault();
        dispatcher.dispatch('file:open');
        return;
      }
    }
  }, false);
}

function setupFileInput(): void {
  const fileInput = document.getElementById('file-input') as HTMLInputElement;

  fileInput.addEventListener('change', async (e) => {
    const input = e.target as HTMLInputElement;
    const skipUnsavedGuard = input.dataset.skipUnsavedGuard === 'true';
    delete input.dataset.skipUnsavedGuard;
    const file = input.files?.[0];
    if (!file) return;
    const name = file.name.toLowerCase();
    if (!name.endsWith('.hwp') && !name.endsWith('.hwpx')) {
      alert('HWP/HWPX 파일만 지원합니다.');
      fileInput.value = '';
      return;
    }
    await loadFile(file, { skipUnsavedGuard });
    fileInput.value = '';
  });

  // 문서 전체에서 브라우저 기본 드롭 동작 방지 (파일 열기/다운로드 방지)
  document.addEventListener('dragover', (e) => e.preventDefault());
  document.addEventListener('drop', (e) => e.preventDefault());

  // 드래그 앤 드롭 지원 (scroll-container 영역)
  const container = document.getElementById('scroll-container')!;
  container.addEventListener('dragover', (e) => {
    e.preventDefault();
    container.classList.add('drag-over');
  });
  container.addEventListener('dragleave', () => {
    container.classList.remove('drag-over');
  });
  container.addEventListener('drop', async (e) => {
    e.preventDefault();
    container.classList.remove('drag-over');
    const file = e.dataTransfer?.files[0];
    if (!file) return;
    const dropName = file.name.toLowerCase();
    const imageExts = ['.png', '.jpg', '.jpeg', '.gif', '.bmp', '.webp'];
    if (imageExts.some(ext => dropName.endsWith(ext))) {
      if (!inputHandler || wasm.pageCount === 0) return;
      const data = new Uint8Array(await file.arrayBuffer());
      const ext = file.name.split('.').pop()?.toLowerCase() || 'png';
      const img = new Image();
      const url = URL.createObjectURL(file);
      try {
        img.src = url;
        await img.decode();
        inputHandler.enterImagePlacementMode(data, ext, img.naturalWidth, img.naturalHeight, file.name);
      } catch {
        console.warn('[drop] 이미지 디코딩 실패:', file.name);
      } finally {
        URL.revokeObjectURL(url);
      }
      return;
    }
    if (!dropName.endsWith('.hwp') && !dropName.endsWith('.hwpx')) {
      alert('HWP/HWPX 파일 또는 이미지 파일만 지원합니다.');
      return;
    }
    await loadFile(file);
  });
}

function setupZoomControls(): void {
  if (!canvasView) return;
  const vm = canvasView.getViewportManager();

  document.getElementById('sb-zoom-in')!.addEventListener('click', () => {
    vm.setZoom(vm.getZoom() + 0.1);
  });
  document.getElementById('sb-zoom-out')!.addEventListener('click', () => {
    vm.setZoom(vm.getZoom() - 0.1);
  });

  // 폭 맞춤: 용지 폭에 맞게 줌 조절
  document.getElementById('sb-zoom-fit-width')!.addEventListener('click', () => {
    if (wasm.pageCount === 0) return;
    const container = document.getElementById('scroll-container')!;
    const containerWidth = container.clientWidth - 40; // 좌우 여백 제외
    const pageInfo = wasm.getPageInfo(0);
    // pageInfo.width는 이미 px 단위 (96dpi 기준)
    const zoom = containerWidth / pageInfo.width;
    console.log(`[zoom-fit-width] container=${containerWidth} page=${pageInfo.width} zoom=${zoom.toFixed(3)}`);
    vm.setZoom(Math.max(0.1, Math.min(zoom, 4.0)));
  });

  // 쪽 맞춤: 한 페이지 전체가 보이도록 줌 조절
  document.getElementById('sb-zoom-fit')!.addEventListener('click', () => {
    if (wasm.pageCount === 0) return;
    const container = document.getElementById('scroll-container')!;
    const containerWidth = container.clientWidth - 40;
    const containerHeight = container.clientHeight - 40;
    const pageInfo = wasm.getPageInfo(0);
    // pageInfo.width/height는 이미 px 단위 (96dpi 기준)
    const zoomW = containerWidth / pageInfo.width;
    const zoomH = containerHeight / pageInfo.height;
    console.log(`[zoom-fit-page] containerW=${containerWidth} containerH=${containerHeight} pageW=${pageInfo.width} pageH=${pageInfo.height} zoomW=${zoomW.toFixed(3)} zoomH=${zoomH.toFixed(3)}`);
    vm.setZoom(Math.max(0.1, Math.min(zoomW, zoomH, 4.0)));
  });

  // 모바일: 줌 값 클릭 → 100% 토글
  document.getElementById('sb-zoom-val')!.addEventListener('click', () => {
    const currentZoom = vm.getZoom();
    if (Math.abs(currentZoom - 1.0) < 0.05) {
      // 현재 100% → 쪽 맞춤으로 전환
      document.getElementById('sb-zoom-fit')!.click();
    } else {
      // 현재 쪽 맞춤/기타 → 100%로 전환
      vm.setZoom(1.0);
    }
  });

  document.addEventListener('keydown', (e) => {
    if (!e.ctrlKey && !e.metaKey) return;
    if (e.key === '=' || e.key === '+') {
      e.preventDefault();
      vm.setZoom(vm.getZoom() + 0.1);
    } else if (e.key === '-') {
      e.preventDefault();
      vm.setZoom(vm.getZoom() - 0.1);
    } else if (e.key === '0') {
      e.preventDefault();
      vm.setZoom(1.0);
    }
  });
}

let totalSections = 1;

function setupEventListeners(): void {
  eventBus.on('current-page-changed', (page, _total) => {
    const pageIdx = page as number;
    sbPage().textContent = `${pageIdx + 1} / ${_total} 쪽`;

    // 구역 정보: 현재 페이지의 sectionIndex로 갱신
    if (wasm.pageCount > 0) {
      try {
        const pageInfo = wasm.getPageInfo(pageIdx);
        sbSection().textContent = `구역: ${pageInfo.sectionIndex + 1} / ${totalSections}`;
      } catch { /* 무시 */ }
    }
  });

  eventBus.on('zoom-level-display', (zoom) => {
    sbZoomVal().textContent = `${Math.round((zoom as number) * 100)}%`;
  });

  // 삽입/수정 모드 토글
  eventBus.on('insert-mode-changed', (insertMode) => {
    document.getElementById('sb-mode')!.textContent = (insertMode as boolean) ? '삽입' : '수정';
  });

  eventBus.on('document-mutated', (reason) => {
    documentState.markDirty(typeof reason === 'string' ? reason : 'document-mutated');
  });

  eventBus.on('document-changed', (reason) => {
    documentState.markDirty(typeof reason === 'string' ? reason : 'document-changed');
  });

  eventBus.on('document-dirty-changed', () => {
    eventBus.emit('command-state-changed');
  });

  // 필드 정보 표시
  const sbField = document.getElementById('sb-field');
  eventBus.on('field-info-changed', (info) => {
    if (!sbField) return;
    const fi = info as { fieldId: number; fieldType: string; guideName?: string } | null;
    if (fi) {
      const label = fi.guideName || `#${fi.fieldId}`;
      sbField.textContent = `[누름틀] ${label}`;
      sbField.style.display = '';
    } else {
      sbField.textContent = '';
      sbField.style.display = 'none';
    }
  });

  // 개체 선택 시 회전/대칭 버튼 그룹 표시/숨김
  const rotateGroup = document.querySelector('.tb-rotate-group') as HTMLElement | null;
  let noteToolbarActive = false;
  if (rotateGroup) {
    eventBus.on('picture-object-selection-changed', (selected) => {
      rotateGroup.style.display = (selected as boolean) && !noteToolbarActive ? '' : 'none';
    });
  }

  // 머리말/꼬리말 편집 모드 시 도구상자 전환 + 본문 dimming
  const hfGroup = document.querySelector('.tb-headerfooter-group') as HTMLElement | null;
  const hfLabel = hfGroup?.querySelector('.tb-hf-label') as HTMLElement | null;
  const noteGroup = document.querySelector('.tb-note-group') as HTMLElement | null;
  const defaultTbGroups = document.querySelectorAll('#icon-toolbar > .tb-group:not(.tb-headerfooter-group):not(.tb-note-group):not(.tb-rotate-group), #icon-toolbar > .tb-sep');
  const scrollContainer = document.getElementById('scroll-container');
  const styleBar = document.getElementById('style-bar');

  eventBus.on('headerFooterModeChanged', (mode) => {
    const isActive = (mode as string) !== 'none';
    // 도구상자 전환
    if (hfGroup) {
      hfGroup.style.display = isActive ? '' : 'none';
    }
    if (hfLabel) {
      hfLabel.textContent = (mode as string) === 'header' ? '머리말' : (mode as string) === 'footer' ? '꼬리말' : '';
    }
    defaultTbGroups.forEach((el) => {
      (el as HTMLElement).style.display = isActive ? 'none' : '';
    });
    // 서식 도구 모음은 머리말/꼬리말 편집 시에도 유지 (문단/글자 모양 설정 필요)
    // 본문 dimming
    if (scrollContainer) {
      if (isActive) {
        scrollContainer.classList.add('hf-editing');
      } else {
        scrollContainer.classList.remove('hf-editing');
      }
    }
  });

  eventBus.on('footnoteModeChanged', (active) => {
    const isActive = active as boolean;
    noteToolbarActive = isActive;
    if (noteGroup) {
      noteGroup.style.display = isActive ? '' : 'none';
    }
    if (rotateGroup && isActive) {
      rotateGroup.style.display = 'none';
    }
    defaultTbGroups.forEach((el) => {
      (el as HTMLElement).style.display = isActive ? 'none' : '';
    });
  });
}

/** 문서 초기화 공통 시퀀스 (loadFile, createNewDocument 양쪽에서 사용) */
async function initializeDocument(docInfo: DocumentInfo, displayName: string): Promise<void> {
  const msg = sbMessage();
  let normalizedDuringLoad = false;
  try {
    console.log('[initDoc] 1. 폰트 로딩 시작');
    if (docInfo.fontsUsed?.length) {
      await loadWebFonts(docInfo.fontsUsed, (loaded, total) => {
        msg.textContent = `폰트 로딩 중... (${loaded}/${total})`;
      });
    }
    console.log('[initDoc] 2. 폰트 로딩 완료');
    msg.textContent = displayName;
    totalSections = docInfo.sectionCount ?? 1;
    sbSection().textContent = `구역: 1 / ${totalSections}`;
    console.log('[initDoc] 3. inputHandler deactivate');
    inputHandler?.deactivate();
    console.log('[initDoc] 4. canvasView loadDocument');
    canvasView?.loadDocument();
    console.log('[initDoc] 5. toolbar setEnabled');
    toolbar?.setEnabled(true);
    console.log('[initDoc] 6. toolbar initFontDropdown + initStyleDropdown');
    toolbar?.initFontDropdown(docInfo.fontsUsed);
    toolbar?.initStyleDropdown();
    console.log('[initDoc] 7. inputHandler activateWithCaretPosition');
    inputHandler?.activateWithCaretPosition();
    console.log('[initDoc] 8. 완료');

    // #177: HWPX 비표준 lineseg 감지 → 경고 있으면 모달로 사용자 선택 요청
    try {
      const report = wasm.getValidationWarnings();
      console.log(`[validation] ${report.count} warnings`, report.summary);
      if (report.count > 0) {
        const choice = await showValidationModalIfNeeded(report);
        console.log(`[validation] user choice: ${choice}`);
        if (choice === 'auto-fix') {
          const n = wasm.reflowLinesegs();
          console.log(`[validation] reflowed ${n} paragraphs`);
          // 렌더 재계산
          canvasView?.loadDocument();
          msg.textContent = `${displayName} (비표준 lineseg ${n}건 자동 보정됨)`;
          normalizedDuringLoad = n > 0;
        }
      }
    } catch (e) {
      console.warn('[validation] 감지/보정 실패 (치명적이지 않음):', e);
    }
    if (normalizedDuringLoad) {
      documentState.markDirty('validation-auto-fix');
    } else {
      documentState.markClean('document-initialized');
    }
  } catch (error) {
    console.error('[initDoc] 오류:', error);
    if (window.innerWidth < 768) alert(`초기화 오류: ${error}`);
  }
}

async function loadFile(file: File, options: { skipUnsavedGuard?: boolean } = {}): Promise<boolean> {
  const msg = sbMessage();
  try {
    if (!options.skipUnsavedGuard) {
      const canReplace = await confirmSaveBeforeReplacingDocument(commandServices);
      if (!canReplace) return false;
    }
    msg.textContent = '파일 로딩 중...';
    const startTime = performance.now();
    const data = new Uint8Array(await file.arrayBuffer());
    await loadBytes(data, file.name, null, startTime);
    return true;
  } catch (error) {
    showLoadError(error);
    return false;
  }
}

async function loadBytes(
  data: Uint8Array,
  fileName: string,
  fileHandle: typeof wasm.currentFileHandle,
  startTime = performance.now(),
): Promise<void> {
  inputHandler?.resetCaptureCoverage();
  const docInfo = wasm.loadDocument(data, fileName);
  wasm.currentFileHandle = fileHandle;
  const elapsed = performance.now() - startTime;
  // initializeDocument 안에서 #177 validation 모달이 표시될 수 있음.
  // HWPX 토스트는 모달과의 이벤트 충돌을 피하기 위해 모달 닫힌 후 표시.
  await initializeDocument(docInfo, `${fileName} — ${docInfo.pageCount}페이지 (${elapsed.toFixed(1)}ms)`);
  notifyHwpxSaveModeIfNeeded();
}

/**
 * #888: HWPX 출처 문서 로드 시 HWP 변환 저장 안내.
 * - 우상단 토스트 1회
 * - 상태 표시줄 메시지
 */
function notifyHwpxSaveModeIfNeeded(): void {
  if (wasm.getSourceFormat() !== 'hwpx') return;

  showToast({
    message: 'HWPX 문서는 저장 시 HWP 형식으로 변환 저장됩니다.\n원본 HWPX를 덮어쓰지 않도록 .hwp 파일명으로 저장합니다.',
    durationMs: 0, // 자동 페이드 없음 — 사용자가 확인 버튼으로 닫음
    action: {
      label: '이슈 보기',
      onClick: () => {
        window.open('https://github.com/edwardkim/rhwp/issues/888', '_blank');
      },
    },
    confirmLabel: '확인',
  });

  const sb = sbMessage();
  if (sb) sb.textContent = 'HWPX 변환 저장 모드 — 저장 시 HWP(.hwp)로 내보냅니다';
}

type DocumentByteKind = 'hwp' | 'hwpx' | 'html' | 'unknown';

const HWP_CFB_SIGNATURE = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] as const;
const ZIP_SIGNATURES = [
  [0x50, 0x4B, 0x03, 0x04],
  [0x50, 0x4B, 0x05, 0x06],
  [0x50, 0x4B, 0x07, 0x08],
] as const;

function startsWithBytes(bytes: Uint8Array, signature: readonly number[]): boolean {
  if (bytes.length < signature.length) return false;
  return signature.every((byte, index) => bytes[index] === byte);
}

function detectDocumentByteKind(bytes: Uint8Array, contentType?: string | null): DocumentByteKind {
  if (startsWithBytes(bytes, HWP_CFB_SIGNATURE)) return 'hwp';
  if (ZIP_SIGNATURES.some(signature => startsWithBytes(bytes, signature))) return 'hwpx';

  const declaredContentType = contentType?.toLowerCase() ?? '';
  if (declaredContentType.includes('text/html')) return 'html';

  const prefix = new TextDecoder('utf-8')
    .decode(bytes.subarray(0, Math.min(bytes.length, 256)))
    .trimStart()
    .toLowerCase();

  if (prefix.startsWith('<!doctype') || prefix.startsWith('<html') || prefix.startsWith('<?xml')) {
    return 'html';
  }

  return 'unknown';
}

function assertRemoteDocumentBytes(bytes: Uint8Array, contentType?: string | null): void {
  const kind = detectDocumentByteKind(bytes, contentType);
  if (kind === 'hwp' || kind === 'hwpx') return;

  if (kind === 'html') {
    throw new Error('실제 HWP/HWPX 파일이 아닙니다. 파일 미리보기/오류 페이지가 반환되었습니다.');
  }

  throw new Error('실제 HWP/HWPX 파일이 아닙니다. 파일 시그니처를 확인할 수 없습니다.');
}

async function createNewDocument(): Promise<void> {
  const msg = sbMessage();
  try {
    msg.textContent = '새 문서 생성 중...';
    const docInfo = wasm.createNewDocument();
    await initializeDocument(docInfo, `새 문서.hwp — ${docInfo.pageCount}페이지`);
  } catch (error) {
    msg.textContent = `새 문서 생성 실패: ${error}`;
    console.error('[main] 새 문서 생성 실패:', error);
  }
}

async function canReplaceCurrentDocument(skipUnsavedGuard?: boolean): Promise<boolean> {
  return skipUnsavedGuard === true || await confirmSaveBeforeReplacingDocument(commandServices);
}

// 커맨드에서 새 문서 생성 호출
eventBus.on('create-new-document', (payload) => {
  void (async () => {
    const options = payload as { skipUnsavedGuard?: boolean } | undefined;
    if (!await canReplaceCurrentDocument(options?.skipUnsavedGuard)) return;
    await createNewDocument();
  })();
});
eventBus.on('open-document-bytes', async (payload) => {
  const data = payload as {
    bytes: Uint8Array;
    fileName: string;
    fileHandle: typeof wasm.currentFileHandle;
    skipUnsavedGuard?: boolean;
    /** 문서 비교 등: 로드 완료를 기다리는 쪽과 짝을 맞출 때만 전달 */
    requestId?: string;
  };
  const notifyDone = (ok: boolean, error?: string) => {
    if (!data.requestId) return;
    eventBus.emit('open-document-bytes:done', { requestId: data.requestId, ok, error });
  };
  try {
    if (!await canReplaceCurrentDocument(data.skipUnsavedGuard)) {
      notifyDone(false, '문서 열기가 취소되었습니다.');
      return;
    }
    await loadBytes(data.bytes, data.fileName, data.fileHandle);
    notifyDone(true);
  } catch (error) {
    // #265: WASM 파서 에러 (예: HWP 3.0 미지원) 를 사용자에게 전파
    showLoadError(error);
    const msg = error instanceof Error ? error.message : String(error);
    notifyDone(false, msg);
  }
});

// 수식 더블클릭 → 수식 편집 대화상자
eventBus.on('equation-edit-request', () => {
  dispatcher.dispatch('insert:equation-edit');
});

/**
 * URL 파라미터(?url=)로 전달된 HWP 파일을 자동 로드한다.
 * Chrome 확장 프로그램에서 뷰어 탭을 열 때 사용.
 */
async function loadFromUrlParam(): Promise<void> {
  const params = new URLSearchParams(window.location.search);
  const fileUrl = params.get('url');
  if (!fileUrl) return;

  const fileName = params.get('filename') || fileUrl.split('/').pop()?.split('?')[0] || 'document.hwp';
  const msg = sbMessage();

  try {
    msg.textContent = '파일 로딩 중...';
    console.log(`[loadFromUrlParam] ${fileUrl}`);

    let response: Response;

    // Chrome 확장 환경: Service Worker를 통한 CORS 우회 fetch
    if (typeof chrome !== 'undefined' && chrome.runtime?.sendMessage) {
      try {
        response = await fetch(fileUrl);
      } catch {
        // 직접 fetch 실패 시 Service Worker 프록시
        const result = await chrome.runtime.sendMessage({ type: 'fetch-file', url: fileUrl });
        if (result.error) throw new Error(result.error);
        const data = new Uint8Array(result.data);
        assertRemoteDocumentBytes(data);
        await loadBytes(data, fileName, null);
        return;
      }
    } else {
      response = await fetch(fileUrl);
    }

    if (!response.ok) throw new Error(`HTTP ${response.status}: ${response.statusText}`);
    const contentType = response.headers.get('content-type');
    const buffer = await response.arrayBuffer();
    const data = new Uint8Array(buffer);
    assertRemoteDocumentBytes(data, contentType);
    await loadBytes(data, fileName, null);
  } catch (error) {
    // 로컬 file:// 로드 실패 + "파일 URL 액세스 허용" 미허용 → 전용 안내 (#1131)
    if (fileUrl.startsWith('file:') && typeof chrome !== 'undefined') {
      const allowed = await isFileSchemeAccessAllowed();
      if (allowed === false) {
        showFileUrlAccessGuidance();
        return;
      }
    }
    showLoadError(error);
  }
}

/**
 * 확장 프로그램의 "파일 URL에 대한 액세스 허용" 권한 상태를 조회한다 (#1131).
 *
 * 확장 페이지에서만 의미가 있다. API 부재(비-확장 환경 등) 시 판정 불가로
 * `null` 을 반환하여 호출부가 기존 동작(일반 에러)으로 폴백하도록 한다.
 *
 * @returns 허용=true, 미허용=false, 판정 불가=null
 */
async function isFileSchemeAccessAllowed(): Promise<boolean | null> {
  const ext = (typeof chrome !== 'undefined' ? chrome.extension : undefined) as
    | { isAllowedFileSchemeAccess?: () => Promise<boolean> }
    | undefined;
  if (!ext?.isAllowedFileSchemeAccess) return null;
  try {
    return await ext.isAllowedFileSchemeAccess();
  } catch {
    return null;
  }
}

/**
 * 로컬 file:// 문서를 열 때 "파일 URL 액세스 허용" 권한이 꺼져 있어 로드가
 * 실패한 경우, 일반 "Failed to fetch" 대신 원인과 해결 방법을 안내한다 (#1131).
 *
 * 설정 화면(chrome://extensions/?id=...)은 일반 링크로는 열리지 않으므로
 * 확장 컨텍스트의 chrome.tabs.create 로 연다.
 */
function showFileUrlAccessGuidance(): void {
  const errMsg = '로컬 파일을 열려면 확장 프로그램의 "파일 URL에 대한 액세스 허용"을 켜야 합니다.\n설정에서 권한을 허용한 뒤 파일을 다시 열어 주세요.';
  const sb = sbMessage();
  if (sb) sb.textContent = '파일 로드 실패: 파일 URL 액세스 권한이 필요합니다.';
  console.error('[main] file:// 로드 실패 — 파일 URL 액세스 미허용 (#1131)');
  showToast({
    message: errMsg,
    durationMs: 0, // 사용자가 읽고 직접 닫기
    confirmLabel: '확인',
    action: {
      label: '설정 열기',
      onClick: () => {
        if (typeof chrome !== 'undefined' && chrome.tabs?.create && chrome.runtime?.id) {
          chrome.tabs.create({ url: `chrome://extensions/?id=${chrome.runtime.id}` });
        }
      },
    },
  });
}

/**
 * 파일 로드 실패 시 사용자에게 에러를 명확히 알린다 (#265).
 *
 * 상태 표시줄은 22px 한 줄로 긴 에러 메시지가 ellipsis 로 잘리므로,
 * 우상단 토스트 (긴 메시지 줄바꿈 지원 · 사용자 닫기 · action 링크) 를
 * 병행 사용한다.
 */
function showLoadError(error: unknown): void {
  const raw = String(error).replace(/^Error:\s*/, '');
  const errMsg = `파일 로드 실패: ${raw}`;
  const sb = sbMessage();
  if (sb) sb.textContent = errMsg;
  console.error('[main] 파일 로드 실패:', error);
  showToast({
    message: errMsg,
    durationMs: 0, // 에러는 자동 페이드 없음 — 사용자가 읽고 닫기
    confirmLabel: '확인',
  });
}

const initPromise = initialize();

// ── iframe 연동 API (postMessage) ──
// 부모 페이지에서 postMessage로 에디터를 제어할 수 있다.
// 요청: { type: 'rhwp-request', id, method, params }
// 응답: { type: 'rhwp-response', id, result?, error? }
window.addEventListener('message', async (e) => {
  const msg = e.data;
  if (!msg || typeof msg !== 'object') return;

  // 기존 hwpctl-load 호환
  if (msg.type === 'hwpctl-load' && msg.data) {
    try {
      await initPromise;
      if (!await canReplaceCurrentDocument(Boolean(msg.skipUnsavedGuard))) {
        e.source?.postMessage({ type: 'rhwp-response', id: msg.id, error: '문서 열기가 취소되었습니다.' }, { targetOrigin: '*' });
        return;
      }
      const bytes = new Uint8Array(msg.data);
      await loadBytes(bytes, msg.fileName || 'document.hwp', null);
      e.source?.postMessage({ type: 'rhwp-response', id: msg.id, result: { pageCount: wasm.pageCount } }, { targetOrigin: '*' });
    } catch (err: any) {
      e.source?.postMessage({ type: 'rhwp-response', id: msg.id, error: err.message || String(err) }, { targetOrigin: '*' });
    }
    return;
  }

  // rhwp-request: 범용 API
  if (msg.type !== 'rhwp-request' || !msg.method) return;
  const { id, method, params } = msg;
  const reply = (result?: any, error?: string) => {
    e.source?.postMessage({ type: 'rhwp-response', id, result, error }, { targetOrigin: '*' });
  };

  try {
    switch (method) {
      case 'ready':
        // wasm 초기화 완료 후에만 true 응답 — race condition 방지 (#522)
        await initPromise;
        reply(true);
        break;
      case 'getRuntimeIdentity':
        reply(runtimeIdentity);
        break;
      case 'getFormattingFingerprint':
        await initPromise;
        reply(buildFormattingFingerprint(params));
        break;
      case 'applyDeterministicEdit':
        await initPromise;
        reply(applyDeterministicEdit(params));
        break;
      case 'loadFile': {
        await initPromise;
        if (!await canReplaceCurrentDocument(Boolean(params?.skipUnsavedGuard))) {
          reply(undefined, '문서 열기가 취소되었습니다.');
          break;
        }
        const bytes = new Uint8Array(params.data);
        await loadBytes(bytes, params.fileName || 'document.hwp', null);
        reply({ pageCount: wasm.pageCount });
        break;
      }
      case 'pageCount':
        await initPromise;
        reply(wasm.pageCount);
        break;
      case 'getPageSvg':
        await initPromise;
        reply(wasm.renderPageSvg(params.page ?? 0));
        break;
      case 'exportHwp':
        await initPromise;
        reply(Array.from(wasm.exportHwp()));
        break;
      case 'exportHwpx':
        await initPromise;
        reply(Array.from(wasm.exportHwpx()));
        break;
      case 'exportHwpVerify':
        await initPromise;
        reply(JSON.parse(wasm.exportHwpVerify()));
        break;
      case 'getFieldList':
        await initPromise;
        reply(wasm.getFieldList());
        break;
      case 'setFieldValueByName': {
        await initPromise;
        const name = String(params?.name ?? '');
        const value = String(params?.value ?? '');
        if (!name) {
          reply(undefined, 'Field name is required.');
          break;
        }
        const result = wasm.setFieldValueByName(name, value);
        if (result.ok === true) {
          inputHandler?.commitExternalDirectMutation('field_value_replace', 'wasm_set_field_value_by_name');
        }
        reply(result);
        break;
      }
      case 'setFieldValue': {
        await initPromise;
        const fieldId = Number(params?.fieldId);
        const value = String(params?.value ?? '');
        if (!Number.isInteger(fieldId)) {
          reply(undefined, 'Field id is required.');
          break;
        }
        const result = wasm.setFieldValue(fieldId, value);
        if (result.ok === true) {
          inputHandler?.commitExternalDirectMutation('field_value_replace', 'wasm_set_field_value');
        }
        reply(result);
        break;
      }
      case 'getClickHereProps':
        await initPromise;
        reply(getClickHereProps(params));
        break;
      case 'updateClickHerePropsForProof':
        await initPromise;
        reply(updateClickHerePropsForProof(params));
        break;
      case 'recordFieldMetadataMutationForProof':
        await initPromise;
        reply(recordFieldMetadataMutationForProof(params));
        break;
      case 'removeFieldByIdForProof':
        await initPromise;
        reply(removeFieldByIdForProof(params));
        break;
      case 'insertHeaderFooterFieldForProof':
        await initPromise;
        reply(insertHeaderFooterFieldForProof(params));
        break;
      case 'getTableCellTextTargets':
        await initPromise;
        reply(getTableCellTextTargets());
        break;
      case 'setTableCellText':
        await initPromise;
        reply(setTableCellText(params));
        break;
      case 'getFootnoteTextTargets':
        await initPromise;
        reply(getFootnoteTextTargets());
        break;
      case 'setFootnoteTextForProof':
        await initPromise;
        reply(setFootnoteTextForProof(params));
        break;
      case 'resizeTableCellForProof':
        await initPromise;
        reply(resizeTableCellForProof(params));
        break;
      case 'getCaptureCoverageObservations':
        await initPromise;
        reply({ observations: inputHandler?.getCaptureCoverageObservations(params ?? {}) ?? [] });
        break;
      case 'runUndoRedoCaptureProof':
        await initPromise;
        reply(runUndoRedoCaptureProof(params));
        break;
      case 'runRecordOnlyCaptureProof':
        await initPromise;
        reply(runRecordOnlyCaptureProof(params));
        break;
      case 'runImeCompositionCaptureProof':
        await initPromise;
        reply(runImeCompositionCaptureProof(params));
        break;
      case 'runIosFallbackInputCaptureProof':
        await initPromise;
        reply(runIosFallbackInputCaptureProof(params));
        break;
      case 'runImagePasteCaptureProof':
        await initPromise;
        reply(runImagePasteCaptureProof(params));
        break;
      case 'runControlPasteCaptureProof':
        await initPromise;
        reply(runControlPasteCaptureProof(params));
        break;
      case 'runInternalPasteCaptureProof':
        await initPromise;
        reply(runInternalPasteCaptureProof(params));
        break;
      default:
        reply(undefined, `Unknown method: ${method}`);
    }
  } catch (err: any) {
    reply(undefined, err.message || String(err));
  }
});
