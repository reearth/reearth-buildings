// Wrangler / esbuild import `.wasm` files as a compiled WebAssembly.Module.
declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}
