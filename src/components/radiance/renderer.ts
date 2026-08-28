import { surface, type Gpu, type Surface } from "vgpu";

import {
  createScene,
  destroyScene,
  prepareScene,
  presentScene,
  renderLighting,
  scaledSize,
  type AgentRadianceScene,
} from "./simulation";

// Fixed "web" preset from the vgpu example: 4-ray cascades, scene capped at
// 640px, 24fps simulation. The hero is a small square, so this is plenty.
const OUTPUT_SCALE = 1;
const MAX_OUTPUT_EDGE = 1920;
const MAX_SCENE_EDGE = 640;
const DIRECTION_BASE = 2;
const FRAMES_PER_SECOND = 24;

interface RendererOptions {
  readonly canvas: HTMLCanvasElement;
}

export function createRenderer({ canvas }: RendererOptions) {
  let disposed = false;
  let gpu: Gpu | undefined;
  let canvasSurface: Surface | undefined;
  let scene: AgentRadianceScene | undefined;
  let scenePrepared = false;
  let sceneGeneration = 0;
  let observer: ResizeObserver | undefined;
  let unsubscribeResize: (() => void) | undefined;
  let animationFrame = 0;
  let resizeFrame = 0;
  let pendingSize:
    | { readonly width: number; readonly height: number }
    | undefined;
  let animationTime = 0;
  let lastTimestamp = 0;
  let lastChainTimestamp = -Infinity;
  let dirty = true;

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    if (animationFrame) cancelAnimationFrame(animationFrame);
    if (resizeFrame) cancelAnimationFrame(resizeFrame);
    let firstError: unknown;
    for (const cleanup of [() => observer?.disconnect(), unsubscribeResize, () => gpu?.dispose()]) {
      try {
        cleanup?.();
      } catch (error) {
        if (firstError === undefined) firstError = error;
      }
    }
    if (firstError !== undefined) throw firstError;
  };

  const fail = (error: unknown): never => {
    try {
      dispose();
    } catch {
      // Keep the operation failure primary after best-effort teardown.
    }
    throw error;
  };

  const rebuildScene = (reportFailure = true) => {
    if (disposed || !gpu || !canvasSurface) return;
    const size = scaledSize(
      canvasSurface.size[0],
      canvasSurface.size[1],
      1,
      MAX_SCENE_EDGE
    );
    if (
      scene?.size[0] === size[0] &&
      scene.size[1] === size[1] &&
      scene.directionBase === DIRECTION_BASE
    ) {
      return;
    }

    const next = createScene(gpu, size, DIRECTION_BASE);
    const previous = scene;
    scene = next;
    scenePrepared = false;
    dirty = true;
    const generation = ++sceneGeneration;
    if (previous) destroyScene(previous);
    const preparation = prepareScene(next, canvasSurface.format).then(() => {
      if (disposed || scene !== next || generation !== sceneGeneration) return;
      scenePrepared = true;
      dirty = true;
    });
    if (reportFailure) {
      void preparation.catch((error: unknown) => {
        if (!disposed && scene === next) fail(error);
      });
    }
    return preparation;
  };

  const onSurfaceResize = () => {
    try {
      rebuildScene();
    } catch (error) {
      fail(error);
    }
  };

  const applyResize = () => {
    resizeFrame = 0;
    const size = pendingSize;
    pendingSize = undefined;
    if (disposed || !size || !canvasSurface) return;
    try {
      canvasSurface.resize(
        scaledSize(size.width, size.height, OUTPUT_SCALE, MAX_OUTPUT_EDGE)
      );
      rebuildScene();
    } catch (error) {
      fail(error);
    }
  };

  const measure = () => {
    const { width, height } = canvas.getBoundingClientRect();
    if (disposed || width <= 0 || height <= 0) return;
    pendingSize = { width, height };
    if (!resizeFrame) resizeFrame = requestAnimationFrame(applyResize);
  };

  const tick = (timestamp: number) => {
    animationFrame = 0;
    if (disposed) return;
    if (!document.hidden && scenePrepared && scene && canvasSurface) {
      const delta =
        lastTimestamp > 0
          ? Math.min((timestamp - lastTimestamp) / 1000, 0.1)
          : 0;
      animationTime += delta;
      const interval = 1000 / FRAMES_PER_SECOND;
      try {
        if (dirty || timestamp - lastChainTimestamp >= interval) {
          renderLighting(scene, animationTime, "final", "center-out");
          dirty = false;
          lastChainTimestamp = timestamp;
        }
        presentScene(scene, canvasSurface, "final");
      } catch (error) {
        fail(error);
      }
    }
    lastTimestamp = timestamp;
    animationFrame = requestAnimationFrame(tick);
  };

  const initialize = async () => {
    const { init } = await import("vgpu");
    if (disposed) return;
    const nextGpu = await init();
    if (disposed) {
      nextGpu.dispose();
      return;
    }
    gpu = nextGpu;
    canvasSurface = surface(gpu, canvas, { autoResize: false, dpr: 1 });
    await rebuildScene(false);
    if (disposed) return;
    unsubscribeResize = canvasSurface.onResize(onSurfaceResize);
    observer =
      typeof ResizeObserver === "undefined"
        ? undefined
        : new ResizeObserver(measure);
    observer?.observe(canvas);
    measure();
    animationFrame = requestAnimationFrame(tick);
  };

  const ready = initialize().catch((error: unknown) => {
    if (!disposed) fail(error);
  });

  return { ready, dispose };
}
