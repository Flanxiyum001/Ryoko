/** One decorative WebGL pass for pending MCP requests in Mission Control.
 * Independent implementation inspired by Louis Studios' Stream Search:
 * https://21st.dev/@louisstudios/components/stream-search
 * No tool inputs, results or privileges are handled here.
 */
const VERTEX = `
attribute vec2 position;
void main() { gl_Position = vec4(position, 0.0, 1.0); }
`;

const FRAGMENT = `
precision mediump float;
uniform vec2 resolution;
uniform float time;
void main() {
  vec2 p = gl_FragCoord.xy / resolution;
  float x = p.x;
  float y = p.y - 0.5;
  vec3 color = vec3(0.0);
  float incoming = 1.0 - smoothstep(0.27, 0.54, x);
  for (int i = 0; i < 6; i++) {
    float f = float(i);
    float curve = (f - 2.5) * 0.125 * pow(max(0.0, 1.0 - x * 2.0), 1.45);
    curve += sin(x * 12.0 - time * 1.4 + f * 1.7) * 0.045 * incoming;
    float d = abs(y - curve);
    float glow = exp(-d * d * 90.0) * 0.16 + exp(-d * d * 1250.0) * 0.23;
    float pulse = 0.65 + 0.35 * sin(x * 19.0 - time * 2.2 + f);
    vec3 tint = mix(vec3(0.23, 0.56, 0.98), vec3(0.68, 0.37, 0.96), f / 5.0);
    color += tint * glow * pulse * incoming;
  }
  float outgoing = smoothstep(0.46, 0.68, x) * (1.0 - smoothstep(0.83, 1.0, x));
  float beam = exp(-y * y * 105.0) * 0.22 + exp(-y * y * 1600.0) * 0.50;
  color += vec3(0.30, 0.85, 0.96) * beam * outgoing * (0.8 + 0.2 * sin(x * 20.0 - time * 2.4));
  gl_FragColor = vec4(min(color, vec3(1.0)), clamp(max(max(color.r, color.g), color.b), 0.0, 0.85));
}
`;

function compile(gl: WebGLRenderingContext, kind: number, source: string): WebGLShader | null {
  const shader = gl.createShader(kind);
  if (!shader) return null;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
  gl.deleteShader(shader);
  return null;
}

function makeRenderer(canvas: HTMLCanvasElement): ((seconds: number) => void) | null {
  try {
    const gl = canvas.getContext('webgl', { alpha: true, premultipliedAlpha: false, antialias: false });
    if (!gl) return null;
    const vertex = compile(gl, gl.VERTEX_SHADER, VERTEX);
    const fragment = compile(gl, gl.FRAGMENT_SHADER, FRAGMENT);
    const program = gl.createProgram();
    const buffer = gl.createBuffer();
    if (!vertex || !fragment || !program || !buffer) {
      if (vertex) gl.deleteShader(vertex);
      if (fragment) gl.deleteShader(fragment);
      if (program) gl.deleteProgram(program);
      if (buffer) gl.deleteBuffer(buffer);
      return null;
    }
    gl.attachShader(program, vertex);
    gl.attachShader(program, fragment);
    gl.linkProgram(program);
    gl.deleteShader(vertex);
    gl.deleteShader(fragment);
    const position = gl.getAttribLocation(program, 'position');
    const resolution = gl.getUniformLocation(program, 'resolution');
    const time = gl.getUniformLocation(program, 'time');
    if (!gl.getProgramParameter(program, gl.LINK_STATUS) || position < 0 || !resolution || !time) {
      gl.deleteProgram(program);
      gl.deleteBuffer(buffer);
      return null;
    }
    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(position);
    gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
    return (seconds) => {
      const width = Math.max(1, Math.min(960, canvas.clientWidth));
      const height = Math.max(1, Math.min(160, canvas.clientHeight));
      if (canvas.width !== width) canvas.width = width;
      if (canvas.height !== height) canvas.height = height;
      gl.viewport(0, 0, width, height);
      gl.uniform2f(resolution, width, height);
      gl.uniform1f(time, seconds);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    };
  } catch {
    return null;
  }
}

interface PendingTool { role: string; tool: string }

export function mountToolStream(before: HTMLElement): { show: (tools: PendingTool[]) => void } {
  const host = document.createElement('div');
  host.className = 'tool-stream';
  host.hidden = true;
  const canvas = document.createElement('canvas');
  canvas.setAttribute('aria-hidden', 'true');
  const label = document.createElement('div');
  label.className = 'tool-stream-label';
  label.setAttribute('role', 'status');
  const title = document.createElement('strong');
  const detail = document.createElement('span');
  label.append(title, detail);
  host.append(canvas, label);
  before.before(host);

  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
  let renderer: ReturnType<typeof makeRenderer> = null;
  let attempted = false;
  let visible = false;
  let frame: number | null = null;
  let last = 0;
  let current = '';
  function stop(): void {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
  }
  function canAnimate(): boolean {
    return !host.hidden && visible && !document.hidden && !reduced.matches
      && document.documentElement.dataset.theme !== 'high-contrast';
  }
  function tick(now: number): void {
    frame = null;
    if (!canAnimate() || !renderer) return;
    if (now - last >= 33) {
      renderer((now % 60000) / 1000);
      last = now;
    }
    frame = requestAnimationFrame(tick);
  }
  function sync(): void {
    if (!canAnimate()) { stop(); return; }
    if (!attempted) {
      attempted = true;
      renderer = makeRenderer(canvas);
      if (renderer) host.dataset.shader = 'ready';
    }
    if (renderer && frame === null) frame = requestAnimationFrame(tick);
  }
  if (typeof IntersectionObserver !== 'undefined') {
    const observer = new IntersectionObserver(([entry]) => {
      visible = entry?.isIntersecting ?? false;
      sync();
    });
    observer.observe(canvas);
  }
  // Without an observer, retain the static CSS fallback rather than render offscreen.
  document.addEventListener('visibilitychange', sync);
  reduced.addEventListener('change', sync);
  canvas.addEventListener('webglcontextlost', (event) => {
    event.preventDefault();
    stop();
    renderer = null;
    delete host.dataset.shader;
  });
  canvas.addEventListener('webglcontextrestored', () => {
    attempted = false;
    sync();
  });
  return {
    show(tools) {
      const next = tools.map((tool) => tool.role + ' · ' + tool.tool).join(' | ');
      if (next !== current) {
        current = next;
        host.hidden = tools.length === 0;
        title.textContent = tools.length === 1 ? 'Tool request pending' : `${tools.length} tool requests pending`;
        detail.textContent = next;
        detail.title = next;
      }
      sync();
    },
  };
}
