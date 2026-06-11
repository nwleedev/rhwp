import type { EditCommand, OperationDescriptor } from './command';
import type { DocumentPosition } from '@/core/types';

export type CaptureCoverageOperationCategory =
  | 'body_text_replace'
  | 'table_cell_text_replace'
  | 'header_footer_text_replace'
  | 'field_value_replace'
  | 'footnote_text_replace'
  | 'complex_paste'
  | 'field_metadata_mutation'
  | 'object_mutation'
  | 'page_setting_mutation';

export type CaptureCoverageSurface =
  | 'command_dispatcher'
  | 'document_event'
  | 'history_record'
  | 'hwpctl_action'
  | 'wasm_direct_mutation';

export type CaptureCoverageVerdict = 'blocked' | 'supported_candidate' | 'unknown' | 'unsupported';

export interface CaptureCoverageObservation {
  category: CaptureCoverageOperationCategory;
  evidenceIds: string[];
  manifestOperationCount: number;
  sourceHook: string;
  surface: CaptureCoverageSurface;
  unsupportedMutations: string[];
  verdict: CaptureCoverageVerdict;
}

export interface CaptureCoverageRequest {
  categories?: CaptureCoverageOperationCategory[];
}

export interface DirectMutationCoverageDescriptor {
  category: CaptureCoverageOperationCategory;
  manifestOperationCount?: number;
  sourceHook: string;
  unsupportedMutations?: string[];
  verdict?: CaptureCoverageVerdict;
}

const textCommandTypes = new Set(['insertText', 'deleteText']);
const complexPasteOperationTypes = new Set([
  'pasteControl',
  'pasteHtml',
  'pasteImage',
  'pasteInternal',
]);
const objectMutationOperationTypes = new Set([
  'cutObject',
  'cutTable',
  'deleteObject',
  'deleteTable',
  'deleteTableColumn',
  'deleteTableRow',
  'insertTableColumn',
  'insertTableRow',
  'mergeTableCells',
  'resizeCellByKeyboard',
  'resizeTableCells',
  'resizeTableProportional',
  'splitTableCell',
  'toggleTableCaption',
]);
const pageSettingOperationTypes = new Set(['columnBreak', 'pageBreak']);

function isTableCellPosition(position: Partial<DocumentPosition> | undefined): boolean {
  if (!position) return false;

  return position.parentParaIndex !== undefined || (position.cellPath?.length ?? 0) > 0;
}

function getCommandPosition(command: EditCommand): Partial<DocumentPosition> | undefined {
  return (command as unknown as { position?: Partial<DocumentPosition> }).position;
}

function getOperationCategory(command: EditCommand): CaptureCoverageOperationCategory | null {
  if (!textCommandTypes.has(command.type)) return null;

  return isTableCellPosition(getCommandPosition(command)) ? 'table_cell_text_replace' : 'body_text_replace';
}

function getUnsupportedSnapshotCategory(desc: OperationDescriptor): CaptureCoverageOperationCategory | null {
  if (desc.kind !== 'snapshot') return null;

  if (desc.meta?.domain === 'page' || pageSettingOperationTypes.has(desc.operationType)) {
    return 'page_setting_mutation';
  }
  if (desc.meta?.domain === 'object' || desc.meta?.domain === 'table' || objectMutationOperationTypes.has(desc.operationType)) {
    return 'object_mutation';
  }
  if (complexPasteOperationTypes.has(desc.operationType)) {
    return 'complex_paste';
  }

  return null;
}

/**
 * Runtime mutation capture coverage observation을 보관한다.
 *
 * 실제 replay manifest payload를 생성하지 않고, host가 어떤 capture surface가 관찰됐는지
 * 평가할 수 있도록 category-level observation만 남긴다.
 */
export class CaptureCoverageCollector {
  private readonly observations: CaptureCoverageObservation[] = [];
  private nextEvidenceId = 1;

  reset(): void {
    this.observations.length = 0;
    this.nextEvidenceId = 1;
  }

  recordOperation(desc: OperationDescriptor): void {
    if (desc.kind === 'snapshot') {
      const category = getUnsupportedSnapshotCategory(desc);
      if (!category) return;

      this.observations.push({
        category,
        evidenceIds: [`runtime-unsupported-${this.nextEvidenceId++}`],
        manifestOperationCount: 0,
        sourceHook: `snapshot_${desc.operationType}`,
        surface: 'command_dispatcher',
        unsupportedMutations: [desc.operationType],
        verdict: 'unsupported',
      });
      return;
    }

    if (desc.kind !== 'command' && desc.kind !== 'record') return;

    const category = getOperationCategory(desc.command);
    if (!category) return;

    this.observations.push({
      category,
      evidenceIds: [`runtime-command-${this.nextEvidenceId++}`],
      manifestOperationCount: 1,
      sourceHook: desc.kind === 'record' ? 'command_history_record' : 'execute_operation_command',
      surface: 'command_dispatcher',
      unsupportedMutations: [],
      verdict: 'supported_candidate',
    });
  }

  recordDirectMutation(desc: DirectMutationCoverageDescriptor): void {
    this.observations.push({
      category: desc.category,
      evidenceIds: [`runtime-direct-${this.nextEvidenceId++}`],
      manifestOperationCount: desc.manifestOperationCount ?? 1,
      sourceHook: desc.sourceHook,
      surface: 'wasm_direct_mutation',
      unsupportedMutations: desc.unsupportedMutations ?? [],
      verdict: desc.verdict ?? 'supported_candidate',
    });
  }

  getObservations(request: CaptureCoverageRequest = {}): CaptureCoverageObservation[] {
    const categories = new Set(request.categories ?? []);
    const shouldFilter = categories.size > 0;

    return this.observations
      .filter((observation) => !shouldFilter || categories.has(observation.category))
      .map((observation) => ({
        ...observation,
        evidenceIds: [...observation.evidenceIds],
        unsupportedMutations: [...observation.unsupportedMutations],
      }));
  }
}
