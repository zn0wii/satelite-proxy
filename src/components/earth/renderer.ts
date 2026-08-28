// Earth globe + starfield scene, ported from vgpu's earth example.
// The globe drops the example's sky pass and bloom chain (the planet caps at
// 0.7, below the 0.71 bright-pass threshold, so bloom only ever held the sun)
// and composites premultiplied-transparent so the page background shows
// through. The starfield keeps the full sky + sun bloom pipeline.
import type { Frame, Gpu, Surface } from "vgpu";
import {
  clock,
  draw,
  effect,
  frame,
  frameLoop,
  geometry,
  sampler,
  surface,
  target,
} from "vgpu";
import { perspectiveCamera, sphere } from "vgpu/scene";

import {
  EARTH_TUNING,
  bloomSize,
  cameraBasis,
  normalizeSize,
  orbitPosition,
  sunDirection,
  type OrbitState,
} from "./planet";

import atmosphereWgsl from "./atmosphere.wgsl";
import bakeCloudsWgsl from "./bake-clouds.wgsl";
import bakeSurfaceWgsl from "./bake-surface.wgsl";
import blurWgsl from "./blur.wgsl";
import brightPassWgsl from "./bright-pass.wgsl";
import compositeGlobeWgsl from "./composite-globe.wgsl";
import compositeWgsl from "./composite.wgsl";
import earthWgsl from "./earth.wgsl";
import overlayWgsl from "./overlay.wgsl";
import skyWgsl from "./sky.wgsl";

const HDR_FORMAT: GPUTextureFormat = "rgba16float";
// The planet caps at 0.7, so an 8-bit sRGB MSAA target is sufficient.
const PLANET_FORMAT: GPUTextureFormat = "rgba8unorm-srgb";
const OPAQUE_BLACK = [0, 0, 0, 1] as const;
const TRANSPARENT = [0, 0, 0, 0] as const;
// Slower than the example's globe-side 4°/s — the backdrop sun should drift,
// not sweep.
const SKY_SUN_DEGREES_PER_SECOND = 1;

/** Live view of the globe's orbit + sun, written by the globe renderer each
    frame and read by the starfield backdrop — the example drives both passes
    from ONE camera, so dragging the globe must rotate the stars with it. */
const sharedGlobe: { yaw: number; pitch: number; sun: number } = {
  yaw: 0,
  pitch: EARTH_TUNING.poster.pitch,
  sun: EARTH_TUNING.poster.sunDegrees,
};

export type GlobeMaps = ReturnType<typeof createMaps>;
export type GlobeScene = ReturnType<typeof createGlobeScene>;
export type GlobeTargets = ReturnType<typeof createGlobeTargets>;
export type SkyScene = ReturnType<typeof createSkyScene>;
export type SkyTargets = ReturnType<typeof createSkyTargets>;

interface GlobeOptions {
  readonly canvas: HTMLCanvasElement;
  /** Enable pointer-drag rotation; off inside buttons where drag must not steal the click. */
  readonly interactive?: boolean;
}

interface SkyOptions {
  readonly canvas: HTMLCanvasElement;
}

export function createGlobeRenderer({ canvas, interactive = true }: GlobeOptions) {
  return runRenderer(canvas, (gpu, canvasSurface) => {
    const maps = createMaps(gpu);
    const scene = createGlobeScene(gpu);
    const targets = createGlobeTargets(gpu, canvasSurface.size);
    setGlobeStaticBindings(scene, maps, targets);
    const preparation = Promise.all([
      bakeMaps(gpu, maps),
      prewarmGlobe(scene, targets, canvasSurface),
    ]);
    const orbit = interactive
      ? installOrbitInput(canvas)
      : staticOrbit();
    const resize = (size: readonly [number, number]) => {
      const full = normalizeSize(size);
      targets.beauty.resize(full);
      targets.planet.resize(full);
      setGlobeSizeBindings(scene, targets);
    };
    const renderFrame = (currentFrame: Frame, deltaTime: number, time: number, sun: number) => {
      const state = orbit.step(deltaTime);
      sharedGlobe.yaw = state.yaw;
      sharedGlobe.pitch = state.pitch;
      sharedGlobe.sun = sun;
      setGlobeFrameUniforms(scene, canvasSurface, state, sun, time);
      currentFrame.pass({ target: targets.planet, clear: TRANSPARENT }, (pass) => {
        pass.draw(scene.earth);
        pass.draw(scene.atmosphere);
      });
      currentFrame.pass({ target: targets.beauty, clear: TRANSPARENT }, (pass) =>
        pass.draw(scene.overlay)
      );
      currentFrame.pass({ target: canvasSurface, clear: TRANSPARENT }, (pass) =>
        pass.draw(scene.composite)
      );
    };
    const initialSun = EARTH_TUNING.poster.sunDegrees;
    return { preparation, orbit, resize, renderFrame, initialSun };
  });
}

export function createStarfieldRenderer({ canvas }: SkyOptions) {
  return runRenderer(canvas, (gpu, canvasSurface) => {
    const scene = createSkyScene(gpu);
    const targets = createSkyTargets(gpu, canvasSurface.size);
    setSkyStaticBindings(scene, targets);
    const preparation = prewarmSky(scene, targets, canvasSurface);
    const orbit = undefined;
    const resize = (size: readonly [number, number]) => {
      const full = normalizeSize(size);
      targets.beauty.resize(full);
      const bloom = bloomSize(full);
      targets.bloomA.resize(bloom);
      targets.bloomB.resize(bloom);
      setSkySizeBindings(scene, targets);
    };
    const renderFrame = (currentFrame: Frame, _deltaTime: number, time: number, _sun: number) => {
      const size = canvasSurface.size;
      // Follow the globe's camera (sharedGlobe) so dragging the earth rotates
      // the stars, and let the sky sun be the planet's sun — one light source.
      // A slow time-based drift keeps the sky turning when the globe is idle
      // (~0.57°/s, a full turn in ~10 min); dragging still shifts it because
      // it writes sharedGlobe.yaw directly.
      const yaw = sharedGlobe.yaw + time * 0.01;
      const basis = cameraBasis(
        orbitPosition({
          yaw,
          pitch: sharedGlobe.pitch,
          radius: EARTH_TUNING.camera.radius,
        }),
        [0, 0, 0],
        EARTH_TUNING.camera.fov
      );
      scene.sky.set({
        sky: {
          right: basis.right,
          tanHalfFov: basis.tanHalfFov,
          up: basis.up,
          aspect: size[0] / Math.max(1, size[1]),
          forward: basis.forward,
          lightDirection: sunDirection(sharedGlobe.sun),
        },
      });
      currentFrame.pass({ target: targets.beauty, clear: OPAQUE_BLACK }, (pass) =>
        pass.draw(scene.sky)
      );
      currentFrame.pass({ target: targets.bloomA, clear: OPAQUE_BLACK }, (pass) =>
        pass.draw(scene.bright)
      );
      const bloomTargets = [targets.bloomB, targets.bloomA] as const;
      scene.blur.forEach((blurPass, index) => {
        currentFrame.pass(
          { target: bloomTargets[index % 2]!, clear: OPAQUE_BLACK },
          (pass) => {
            pass.draw(blurPass);
          }
        );
      });
      currentFrame.pass({ target: canvasSurface, clear: OPAQUE_BLACK }, (pass) =>
        pass.draw(scene.composite)
      );
    };
    return { preparation, orbit, resize, renderFrame, initialSun: 0 };
  });
}

// Shared browser lifecycle: gpu init, resize plumbing, frame loop, disposal.
// `build` creates the per-renderer resources and per-frame draw callbacks.
function runRenderer(
  canvas: HTMLCanvasElement,
  build: (
    gpu: Gpu,
    canvasSurface: Surface
  ) => {
    preparation: Promise<unknown>;
    orbit?: { step(deltaTime: number): OrbitState; dispose(): void };
    resize(size: readonly [number, number]): void;
    renderFrame(currentFrame: Frame, deltaTime: number, time: number, sun: number): void;
    initialSun: number;
  }
) {
  let disposed = false;
  let gpu: Gpu | undefined;
  let orbit: { step(deltaTime: number): OrbitState; dispose(): void } | undefined;
  let observer: ResizeObserver | undefined;
  let resizeFrame = 0;
  let pendingSize: readonly [number, number] | undefined;
  let lastDpr = typeof window === "undefined" ? 1 : window.devicePixelRatio;

  const applyResize = () => {
    resizeFrame = 0;
    const size = pendingSize;
    pendingSize = undefined;
    if (disposed || !size) return;
    try {
      built?.resize(size);
    } catch (error) {
      fail(error);
    }
  };
  const measure = () => {
    const { width, height } = canvas.getBoundingClientRect();
    if (disposed || width <= 0 || height <= 0) return;
    const dpr = Math.min(2, Math.max(1, window.devicePixelRatio || 1));
    pendingSize = [
      Math.max(1, Math.round(width * dpr)),
      Math.max(1, Math.round(height * dpr)),
    ];
    if (!resizeFrame) resizeFrame = requestAnimationFrame(applyResize);
  };
  const onWindowResize = () => {
    if (window.devicePixelRatio === lastDpr) return;
    lastDpr = window.devicePixelRatio;
    measure();
  };
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    if (resizeFrame) cancelAnimationFrame(resizeFrame);
    observer?.disconnect();
    if (typeof window !== "undefined")
      window.removeEventListener("resize", onWindowResize);
    orbit?.dispose();
    gpu?.dispose();
  };
  const fail = (error: unknown): never => {
    try {
      dispose();
    } catch {
      // Keep the operation failure primary after best-effort teardown.
    }
    throw error;
  };

  let built: ReturnType<typeof build> | undefined;
  let canvasSurface: Surface | undefined;

  const initialize = async () => {
    const { init } = await import("vgpu");
    if (disposed) return;
    const nextGpu = await init();
    if (disposed) {
      nextGpu.dispose();
      return;
    }
    gpu = nextGpu;
    canvasSurface = surface(gpu, canvas, { dpr: [1, 2] });
    built = build(gpu, canvasSurface);
    orbit = built.orbit;
    await built.preparation;
    if (disposed) return;
    let sunDegrees = built.initialSun;
    observer =
      typeof ResizeObserver === "undefined"
        ? undefined
        : new ResizeObserver(measure);
    observer?.observe(canvas);
    window.addEventListener("resize", onWindowResize);
    measure();
    const time = clock(gpu);
    frameLoop(gpu, (currentFrame) => {
      if (disposed || !built) return;
      try {
        const deltaTime = Math.min(0.05, time.deltaTime);
        const degreesPerSecond = orbit ? EARTH_TUNING.sun.degreesPerSecond : SKY_SUN_DEGREES_PER_SECOND;
        sunDegrees = (sunDegrees + deltaTime * degreesPerSecond) % 360;
        built.renderFrame(currentFrame, deltaTime, time.time, sunDegrees);
      } catch (error) {
        fail(error);
      }
    });
  };
  const ready = initialize().catch((error: unknown) => {
    if (!disposed) fail(error);
  });

  return { ready, dispose };
}

function createMaps(gpu: Gpu) {
  return createResourceGraph((own) => {
    const size = EARTH_TUNING.maps.size;
    return {
      // sRGB preserves precision in dark oceans while alpha stays linear.
      surface: own(target(gpu, { size, format: PLANET_FORMAT })),
      clouds: own(target(gpu, { size, format: "r8unorm" })),
    };
  });
}

function createGlobeScene(gpu: Gpu) {
  return createResourceGraph((own) => {
    const earthGeometry = own(geometry(gpu, sphere(EARTH_TUNING.planet)));
    const atmosphereGeometry = own(
      geometry(gpu, sphere(EARTH_TUNING.atmosphere))
    );
    return {
      earthGeometry,
      atmosphereGeometry,
      earth: draw(gpu, { shader: earthWgsl, geometry: earthGeometry }),
      // Alpha blending turns the shell's fresnel into a rim glow.
      atmosphere: draw(gpu, {
        shader: atmosphereWgsl,
        geometry: atmosphereGeometry,
        blend: "alpha",
      }),
      overlay: effect(gpu, overlayWgsl, { blend: "premultiplied" }),
      composite: effect(gpu, compositeGlobeWgsl),
      mapSampler: sampler(gpu, {
        minFilter: "linear",
        magFilter: "linear",
        // Longitude wraps, latitude does not.
        addressModeU: "repeat",
        addressModeV: "clamp-to-edge",
      }),
      linearSampler: sampler(gpu, { minFilter: "linear", magFilter: "linear" }),
    };
  });
}

function createGlobeTargets(gpu: Gpu, size: readonly [number, number]) {
  return createResourceGraph((own) => {
    const full = normalizeSize(size);
    return {
      beauty: own(target(gpu, { size: full, format: HDR_FORMAT })),
      planet: own(
        target(gpu, {
          size: full,
          format: PLANET_FORMAT,
          msaa: true,
          depth: true,
        })
      ),
    };
  });
}

function setGlobeStaticBindings(
  scene: GlobeScene,
  maps: GlobeMaps,
  targets: GlobeTargets
): void {
  scene.earth.set({
    surfaceMap: maps.surface,
    cloudMap: maps.clouds,
    mapSampler: scene.mapSampler,
  });
  scene.overlay.set({
    planetTexture: targets.planet,
    samp: scene.linearSampler,
  });
  scene.composite.set({ samp: scene.linearSampler });
  setGlobeSizeBindings(scene, targets);
}

function setGlobeSizeBindings(scene: GlobeScene, targets: GlobeTargets): void {
  scene.composite.set({ beauty: targets.beauty });
}

// The procedural maps are baked once in a single submit.
async function bakeMaps(gpu: Gpu, maps: GlobeMaps): Promise<void> {
  const surfacePass = effect(gpu, bakeSurfaceWgsl);
  const cloudPass = effect(gpu, bakeCloudsWgsl);
  await Promise.all([
    surfacePass.compile(maps.surface),
    cloudPass.compile(maps.clouds),
  ]);
  frame(gpu, (currentFrame) => {
    currentFrame.pass({ target: maps.surface, clear: TRANSPARENT }, (pass) =>
      pass.draw(surfacePass)
    );
    currentFrame.pass({ target: maps.clouds, clear: TRANSPARENT }, (pass) =>
      pass.draw(cloudPass)
    );
  });
}

async function prewarmGlobe(
  scene: GlobeScene,
  targets: GlobeTargets,
  output: Surface
): Promise<void> {
  await Promise.all([
    scene.earth.compile(targets.planet),
    scene.atmosphere.compile(targets.planet),
    scene.overlay.compile(targets.beauty),
    scene.composite.compile({ colors: [output.format] }),
  ]);
}

function setGlobeFrameUniforms(
  scene: GlobeScene,
  output: Surface,
  orbit: OrbitState,
  sunDegrees: number,
  time: number
): void {
  const { camera, atmosphere } = EARTH_TUNING;
  const aspect = output.size[0] / Math.max(1, output.size[1]);
  const position = orbitPosition(orbit);
  const light = sunDirection(sunDegrees);
  const view = perspectiveCamera({
    fov: camera.fov,
    aspect,
    near: camera.near,
    far: camera.far,
    position,
    target: [0, 0, 0],
  });

  scene.earth.set({
    earth: {
      viewProjection: view.viewProjection,
      cameraPosition: position,
      time,
      lightDirection: light,
    },
  });
  scene.atmosphere.set({
    atmosphere: {
      viewProjection: view.viewProjection,
      cameraPosition: position,
      strength: atmosphere.strength,
      lightDirection: light,
      _pad: 0,
    },
  });
}

function createSkyScene(gpu: Gpu) {
  return createResourceGraph(() => {
    return {
      sky: effect(gpu, skyWgsl),
      bright: effect(gpu, brightPassWgsl),
      // Each encoded blur needs its own uniform buffer.
      blur: [
        effect(gpu, blurWgsl),
        effect(gpu, blurWgsl),
        effect(gpu, blurWgsl),
        effect(gpu, blurWgsl),
      ] as const,
      composite: effect(gpu, compositeWgsl),
      linearSampler: sampler(gpu, { minFilter: "linear", magFilter: "linear" }),
    };
  });
}

function createSkyTargets(gpu: Gpu, size: readonly [number, number]) {
  return createResourceGraph((own) => {
    const full = normalizeSize(size);
    const bloom = bloomSize(full);
    return {
      beauty: own(target(gpu, { size: full, format: HDR_FORMAT })),
      bloomA: own(target(gpu, { size: bloom, format: HDR_FORMAT })),
      bloomB: own(target(gpu, { size: bloom, format: HDR_FORMAT })),
    };
  });
}

function setSkyStaticBindings(scene: SkyScene, targets: SkyTargets): void {
  const { bloom } = EARTH_TUNING;
  scene.bright.set({ samp: scene.linearSampler });
  const directions = [
    [1, 0],
    [0, 1],
  ] as const;
  scene.blur.forEach((pass, index) => {
    pass.set({
      samp: scene.linearSampler,
      blur: {
        direction: directions[index % 2]!,
        radius: bloom.radii[Math.floor(index / 2)],
      },
    });
  });
  scene.composite.set({
    samp: scene.linearSampler,
    bloom: targets.bloomA,
  });
  setSkySizeBindings(scene, targets);
}

function setSkySizeBindings(scene: SkyScene, targets: SkyTargets): void {
  scene.bright.set({ src: targets.beauty });
  const sources = [targets.bloomA, targets.bloomB] as const;
  scene.blur.forEach((pass, index) => {
    const src = sources[index % 2]!;
    pass.set({ src, blur: { texelSize: src.texelSize } });
  });
  scene.composite.set({ beauty: targets.beauty });
}

async function prewarmSky(
  scene: SkyScene,
  targets: SkyTargets,
  output: Surface
): Promise<void> {
  await Promise.all([
    scene.sky.compile(targets.beauty),
    scene.bright.compile(targets.bloomA),
    ...scene.blur.map((pass, index) =>
      pass.compile(index % 2 === 0 ? targets.bloomB : targets.bloomA)
    ),
    scene.composite.compile({ colors: [output.format] }),
  ]);
}

function staticOrbit() {
  const fixed: OrbitState = {
    yaw: 0,
    pitch: EARTH_TUNING.poster.pitch,
    radius: EARTH_TUNING.camera.radius,
  };
  return {
    step(): OrbitState {
      return fixed;
    },
    dispose() {},
  };
}

function installOrbitInput(canvas: HTMLCanvasElement) {
  const initial = {
    yaw: 0,
    pitch: EARTH_TUNING.poster.pitch,
    radius: EARTH_TUNING.camera.radius,
  };
  let { yaw, pitch } = initial;
  let targetYaw = yaw;
  let targetPitch: number = pitch;
  let activePointer: number | undefined;
  let lastX = 0;
  let lastY = 0;
  const previousTouchAction = canvas.style.touchAction;
  canvas.style.touchAction = "none";

  const down = (event: PointerEvent) => {
    if (!event.isPrimary || activePointer !== undefined) return;
    activePointer = event.pointerId;
    lastX = event.clientX;
    lastY = event.clientY;
    canvas.setPointerCapture?.(event.pointerId);
  };
  const move = (event: PointerEvent) => {
    if (!event.isPrimary || event.pointerId !== activePointer) return;
    const rect = canvas.getBoundingClientRect();
    targetYaw -=
      ((event.clientX - lastX) / Math.max(1, rect.width)) * Math.PI * 2;
    targetPitch +=
      ((event.clientY - lastY) / Math.max(1, rect.height)) * Math.PI;
    targetPitch = Math.max(-1.45, Math.min(1.45, targetPitch));
    lastX = event.clientX;
    lastY = event.clientY;
  };
  const up = (event: PointerEvent) => {
    if (event.pointerId !== activePointer) return;
    if (canvas.hasPointerCapture?.(event.pointerId)) {
      canvas.releasePointerCapture(event.pointerId);
    }
    activePointer = undefined;
  };
  canvas.addEventListener("pointerdown", down);
  canvas.addEventListener("pointermove", move);
  canvas.addEventListener("pointerup", up);
  canvas.addEventListener("pointercancel", up);

  return {
    step(deltaTime: number): OrbitState {
      const blend = 1 - Math.exp(-deltaTime * 9);
      yaw += (targetYaw - yaw) * blend;
      pitch += (targetPitch - pitch) * blend;
      return { yaw, pitch, radius: initial.radius };
    },
    dispose() {
      canvas.removeEventListener("pointerdown", down);
      canvas.removeEventListener("pointermove", move);
      canvas.removeEventListener("pointerup", up);
      canvas.removeEventListener("pointercancel", up);
      if (
        activePointer !== undefined &&
        canvas.hasPointerCapture?.(activePointer)
      ) {
        canvas.releasePointerCapture(activePointer);
      }
      canvas.style.touchAction = previousTouchAction;
    },
  };
}

function createResourceGraph<T>(
  build: (own: <R extends object>(resource: R) => R) => T
): T {
  const resources: object[] = [];
  try {
    return build((resource) => {
      resources.push(resource);
      return resource;
    });
  } catch (error) {
    try {
      destroyResources(resources);
    } catch {
      // Preserve the construction failure after attempting every rollback.
    }
    throw error;
  }
}

function destroyResources(resources: readonly object[]): void {
  let firstError: unknown;
  let failed = false;
  for (let i = resources.length - 1; i >= 0; i--) {
    try {
      (resources[i] as { destroy?: () => void }).destroy?.();
    } catch (error) {
      if (!failed) firstError = error;
      failed = true;
    }
  }
  if (failed) throw firstError;
}
