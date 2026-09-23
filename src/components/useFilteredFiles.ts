/**
 * Client-side filtering for the report's changed-file list.
 *
 * The report is collected once by the backend; search + layer/risk filters are
 * pure UI state so they never re-trigger a collect. The filter logic lives in
 * `filterFiles` (pure) so it can be unit-tested in isolation; `useFilteredFiles`
 * just wires it to React state.
 */
import { useMemo } from "react";
import type { FileChange } from "./testReportTypes";

export interface FileFilter {
  search: string;
  layers: string[];
  risks: string[];
}

export const EMPTY_FILTER: FileFilter = { search: "", layers: [], risks: [] };

export function normalizeSearch(raw: string): string {
  return raw.trim().toLowerCase();
}

/** Pure filter: substring on path, plus multi-select layer and risk. */
export function filterFiles(files: FileChange[], filter: FileFilter): FileChange[] {
  const needle = normalizeSearch(filter.search);
  return files.filter((file) => {
    if (needle && !file.path.toLowerCase().includes(needle)) return false;
    if (filter.layers.length > 0 && !filter.layers.includes(file.layer)) return false;
    if (filter.risks.length > 0 && !filter.risks.some((r) => file.risks.includes(r))) return false;
    return true;
  });
}

/** Distinct, sorted options derived from the current report's files. */
export function collectFilterOptions(files: FileChange[]): { layers: string[]; risks: string[] } {
  const layerSet = new Set<string>();
  const riskSet = new Set<string>();
  for (const file of files) {
    if (file.layer) layerSet.add(file.layer);
    for (const risk of file.risks) riskSet.add(risk);
  }
  const sort = (a: string, b: string) => a.localeCompare(b);
  return { layers: [...layerSet].sort(sort), risks: [...riskSet].sort(sort) };
}

export interface FilteredFilesResult {
  visible: FileChange[];
  total: number;
  options: { layers: string[]; risks: string[] };
}

export function useFilteredFiles(files: FileChange[], filter: FileFilter): FilteredFilesResult {
  return useMemo(() => {
    const options = collectFilterOptions(files);
    const visible = filterFiles(files, filter);
    return { visible, total: files.length, options };
  }, [files, filter]);
}
