// Just enough of bun:ffi for the type check; the real types come from Bun.
declare module 'bun:ffi' {
  export const suffix: string
  export const FFIType: Record<string, number>
  export function ptr(view: Uint8Array): number
  export function dlopen(path: string, symbols: Record<string, unknown>): {
    symbols: Record<string, (...args: any[]) => any>
    close(): void
  }
}
