/// <reference types="vite/client" />
/// <reference types="@webgpu/types" />

// What @vgpu/wgsl's vite loader emits for a `*.wgsl` import: the v1 artifact,
// not a bare string (mirrors @vgpu/wgsl's own wgsl-types.d.ts).
declare module "*.wgsl" {
  const source: { readonly version: 1; readonly wgsl: string };
  export default source;
}
