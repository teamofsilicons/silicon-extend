import { onCleanup, onMount } from "solid-js";

/**
 * Where the pixels thin out.
 * - `field`: a printed block whose edges thin out through the print's ordered screen on every side.
 * - `orb`: a round colour study for empty states.
 * - `ticket`: a solid block that breaks up towards the ticket's tear line (right edge, or the
 *   bottom edge when the block is a wide band).
 */
export type ShaderVariant = "field" | "orb" | "ticket";

const VARIANT: Record<ShaderVariant, number> = { field: 0, orb: 1, ticket: 2 };

/**
 * A still colour study in Silicon blue, after Interface's Shader: cobalt into pale sky with a warm
 * horizon and grain. Extend prints it: four inks (sky, periwinkle, cobalt, deep blue) and a warm
 * horizon, laid down through an ordered (Bayer) dither in square pixels, with edges that break up
 * into scattered pixels, like a risograph pull. On first sight it
 * resolves pixel by pixel; with reduced motion it appears at once. Without WebGL the CSS fallback
 * gradient in styles.css shows instead.
 */
export default function Shader(props: { variant?: ShaderVariant; class?: string; seed?: number; /** Print cell in CSS pixels (default 4). */ cell?: number }) {
  let canvas!: HTMLCanvasElement;
  onMount(() => {
    const gl = canvas.getContext("webgl", {
      alpha: true,
      premultipliedAlpha: false,
      antialias: false,
      powerPreference: "low-power",
      preserveDrawingBuffer: true,
    });
    if (!gl) {
      canvas.dataset.ready = "false";
      return;
    }
    let program: WebGLProgram | null = null;
    let buffer: WebGLBuffer | null = null;
    const uniforms: Record<string, WebGLUniformLocation | null> = {};
    let inView = false;
    let lost = false;
    let disposed = false;
    let frame = 0;
    let revealStart = 0;
    let reveal = 0;
    const still = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

    const release = () => {
      gl.deleteBuffer(buffer);
      gl.deleteProgram(program);
      buffer = program = null;
    };
    const build = () => {
      release();
      const shaders: WebGLShader[] = [];
      const compile = (kind: number, source: string) => {
        const shader = gl.createShader(kind);
        if (!shader) return null;
        shaders.push(shader);
        gl.shaderSource(shader, source);
        gl.compileShader(shader);
        return gl.getShaderParameter(shader, gl.COMPILE_STATUS) ? shader : null;
      };
      const vertex = compile(gl.VERTEX_SHADER, "attribute vec2 a;void main(){gl_Position=vec4(a,0.,1.);}");
      const fragment = compile(
        gl.FRAGMENT_SHADER,
        `precision mediump float;
        uniform vec2 resolution;
        uniform float pixelRatio;
        uniform float reveal;
        uniform float variant;
        uniform float seed;
        uniform float cellSize;
        float grain(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}
        float bayer2(vec2 a){a=floor(a);return fract(dot(a,vec2(.5,a.y*.75)));}
        float bayer4(vec2 a){return bayer2(.5*a)*.25+bayer2(a);}
        float bayer8(vec2 a){return bayer4(.5*a)*.25+bayer2(a);}
        void main(){
          float size=cellSize*pixelRatio;
          vec2 cell=floor(gl_FragCoord.xy/size);
          vec2 uv=(cell+.5)*size/resolution;
          float order=bayer8(cell);

          float wave=sin(uv.x*3.8+seed)*.07;
          float height=smoothstep(.06,.78,uv.y+wave-(uv.x-.5)*.2);
          float tone=clamp(height+smoothstep(.72,1.,uv.y)*.3+(grain(cell+seed)-.5)*.08,0.,1.);
          vec3 sky=vec3(.64,.82,.85);
          vec3 mid=vec3(.43,.57,.86);
          vec3 blue=vec3(.09,.21,.72);
          vec3 deep=vec3(.06,.16,.56);
          vec3 warm=vec3(.95,.6,.42);
          float level=tone*3.;
          float ink=floor(level)+step(order,fract(level));
          vec3 color=mix(sky,mid,step(.5,ink));
          color=mix(color,blue,step(1.5,ink));
          color=mix(color,deep,step(2.5,ink));
          float horizon=exp(-pow((uv.y-.24-uv.x*.03+wave*.4)*20.,2.));
          color=mix(color,warm,step(fract(order+.37),horizon*.7));

          float cover=1.;
          float scatter=grain(cell*1.37+seed*3.1);
          if(variant<.5){
            // The field's edges thin out through the same ordered screen as its inks (a print's
            // falloff), with a little noise so they don't read as a grid.
            vec2 e=min(uv,1.-uv);
            cover=smoothstep(0.,.12,e.x)*smoothstep(0.,.15,e.y);
            scatter=mix(order,scatter,.35);
          }else if(variant<1.5){
            vec2 p=(uv-.5)*vec2(resolution.x/resolution.y,1.);
            cover=1.-smoothstep(.16,.5,length(p));
          }else{
            float along=resolution.x>resolution.y*1.6?uv.y:1.-uv.x;
            cover=smoothstep(0.,.42,along);
          }
          float on=step(scatter,cover*reveal*1.02);
          gl_FragColor=vec4(color,on);
        }`,
      );
      if (!vertex || !fragment) {
        shaders.forEach((shader) => gl.deleteShader(shader));
        return false;
      }
      program = gl.createProgram();
      if (!program) {
        shaders.forEach((shader) => gl.deleteShader(shader));
        return false;
      }
      gl.attachShader(program, vertex);
      gl.attachShader(program, fragment);
      gl.linkProgram(program);
      shaders.forEach((shader) => gl.deleteShader(shader));
      if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
        release();
        return false;
      }
      buffer = gl.createBuffer();
      if (!buffer) {
        release();
        return false;
      }
      gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), gl.STATIC_DRAW);
      gl.useProgram(program);
      const position = gl.getAttribLocation(program, "a");
      gl.enableVertexAttribArray(position);
      gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
      for (const name of ["resolution", "pixelRatio", "reveal", "variant", "seed", "cellSize"]) uniforms[name] = gl.getUniformLocation(program, name);
      return true;
    };
    const draw = () => {
      if (disposed || lost || !program) return;
      const scale = Math.min(window.devicePixelRatio || 1, 1.5);
      const width = Math.max(1, Math.round(canvas.clientWidth * scale));
      const height = Math.max(1, Math.round(canvas.clientHeight * scale));
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      gl.viewport(0, 0, width, height);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.uniform2f(uniforms.resolution, width, height);
      gl.uniform1f(uniforms.pixelRatio, scale);
      gl.uniform1f(uniforms.reveal, reveal);
      gl.uniform1f(uniforms.variant, VARIANT[props.variant ?? "field"]);
      gl.uniform1f(uniforms.seed, props.seed ?? 1.7);
      gl.uniform1f(uniforms.cellSize, props.cell ?? 4);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    };
    /** The pixel-by-pixel entrance: 0.8 s, once, the first time the canvas is on screen. */
    const step = (time: number) => {
      if (disposed) return;
      if (!revealStart) revealStart = time;
      const t = Math.min(1, (time - revealStart) / 800);
      reveal = 1 - Math.pow(1 - t, 3);
      draw();
      frame = t < 1 ? requestAnimationFrame(step) : 0;
    };
    const refresh = () => {
      if (document.hidden || !inView) return;
      if (reveal === 0 && !frame) {
        if (still) reveal = 1;
        else {
          frame = requestAnimationFrame(step);
          return;
        }
      }
      draw();
    };
    const contextLost = (event: Event) => {
      event.preventDefault();
      lost = true;
      cancelAnimationFrame(frame);
      frame = 0;
      canvas.dataset.ready = "false";
    };
    const contextRestored = () => {
      lost = false;
      reveal = 1;
      canvas.dataset.ready = String(build());
      refresh();
    };
    canvas.dataset.ready = String(build());
    const observer = new IntersectionObserver(([entry]) => {
      inView = entry.isIntersecting;
      refresh();
    });
    observer.observe(canvas);
    const resize = new ResizeObserver(refresh);
    resize.observe(canvas);
    canvas.addEventListener("webglcontextlost", contextLost);
    canvas.addEventListener("webglcontextrestored", contextRestored);
    document.addEventListener("visibilitychange", refresh);
    onCleanup(() => {
      disposed = true;
      cancelAnimationFrame(frame);
      observer.disconnect();
      resize.disconnect();
      canvas.removeEventListener("webglcontextlost", contextLost);
      canvas.removeEventListener("webglcontextrestored", contextRestored);
      document.removeEventListener("visibilitychange", refresh);
      release();
    });
  });
  return <canvas ref={canvas} class={`shader shader-${props.variant ?? "field"} ${props.class ?? ""}`} aria-hidden="true" />;
}
