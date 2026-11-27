export interface ImageInfo {
  width: number
  height: number
  /** 1 gray, 2 gray+alpha, 3 RGB, 4 RGBA */
  channels: number
  depth: number
  /** channels actually coded after removing constant alpha / gray colour */
  codedChannels: number
  source: 'png' | 'bmp'
  mode: number
  paletteSize: number
}

/** A JPEG (or .komla file holding one). */
export interface JpegInfo {
  width: number
  height: number
  components: number
  progressive: boolean
  /** 0 stored as is, 1 stored with the byte model, 2 recompressed, 3 recompressed in the fast
   * mode, 4 stored with the fast byte model */
  mode: number
  stripes: number
  /** chroma subsampling, e.g. "4:2:0" or "gray" */
  sampling: string
  /** why a file was stored instead of recompressed */
  note: string
}

export const JPEG_MODE_RECOMPRESSED = 2
export const JPEG_MODE_FAST = 3
export const JPEG_MODE_FAST_STORED = 4

export type WorkerRequest =
  | { op: 'prepare'; file: ArrayBuffer }
  | { op: 'encodeStripe'; header: Uint8Array; rows: Uint8Array; height: number }
  | { op: 'parse'; file: Uint8Array }
  | { op: 'decodeStripe'; header: Uint8Array; data: Uint8Array; height: number }
  | { op: 'finish'; header: Uint8Array; coded: Uint8Array; wantPng: boolean }
  | { op: 'readImage'; file: ArrayBuffer }
  | { op: 'jpegPrepare'; file: ArrayBuffer }
  | { op: 'jpegEncodeStripe'; blob: Uint8Array; coefs: Uint8Array; k: number }
  | { op: 'jpegParse'; file: Uint8Array }
  | { op: 'jpegDecodeStripe'; blob: Uint8Array; data: Uint8Array; k: number }
  | { op: 'jpegFinish'; file: Uint8Array; coefs: Uint8Array }
  | { op: 'jpegCompressFast'; file: ArrayBuffer }
  | { op: 'jpegDecompress'; file: Uint8Array }

export interface JpegPrepared {
  info: JpegInfo
  /** container bytes before the stripe table (the whole .komla file if the JPEG is stored) */
  header: Uint8Array
  /** the JPEG's marker segments, needed by every stripe coder */
  blob: Uint8Array
  /** coefficients of each stripe (i16 little-endian) */
  stripes: Uint8Array[]
}

export interface JpegParsed {
  info: JpegInfo
  blob: Uint8Array
  stripes: { offset: number; length: number }[]
}

export interface Candidate {
  header: Uint8Array
  stripes: [number, number][]
  info: ImageInfo
}

export interface Prepared {
  candidates: Candidate[]
  samples: Uint8Array
  coded: Uint8Array
}

export interface ParsedFile {
  headerLen: number
  stripes: { offset: number; length: number; y0: number; y1: number }[]
  info: ImageInfo
}

export type BaselineRequest =
  | { op: 'zip'; samples: Uint8Array; width: number; height: number; channels: number; depth: number }
  | { op: 'webp'; rgba: Uint8ClampedArray; width: number; height: number }
  | { op: 'jxl'; rgba: Uint8ClampedArray; width: number; height: number; effort: number }
