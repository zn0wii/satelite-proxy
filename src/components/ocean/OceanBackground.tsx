import { useEffect, useRef } from "react";
import { createRenderer } from "./renderer";

/**
 * FFT ocean (ported from vgpu's fft-ocean example) as a full-viewport
 * backdrop behind the dashboard. Mounted via OceanBackgroundLazy so the
 * vgpu chunk only loads when it actually renders.
 */
export function OceanBackground() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const renderer = createRenderer({ canvas });
    // Purely decorative: WebGPU missing or any init/render failure must
    // degrade to the plain theme gradient, never surface an error modal.
    renderer.ready.catch(() => {});
    return () => renderer.dispose();
  }, []);

  return <canvas ref={canvasRef} className="ocean-bg" aria-hidden />;
}
