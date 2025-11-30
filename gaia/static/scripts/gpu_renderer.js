/**
 * Gaia GPU Renderer (WebGL 2.0)
 * File: gaia/static/scripts/gpu-renderer.js
 * Purpose: Hardware-accelerated rendering with GPU optimization
 */

class GPURenderer {
  constructor(options = {}) {
    this.canvas = null;
    this.context = null;
    this.program = null;
    this.shaders = new Map();
    this.textures = new Map();
    this.frameCount = 0;
    this.fps = 0;
    this.lastTime = performance.now();
    this.enabled = options.enabled !== false;
  }

  /**
   * Initialize WebGL 2.0 context
   */
  async initialize(canvas) {
    try {
      this.canvas = canvas || document.createElement('canvas');
      this.canvas.width = window.innerWidth;
      this.canvas.height = window.innerHeight;

      // Get WebGL 2.0 context
      this.context = this.canvas.getContext('webgl2', {
        antialias: true,
        alpha: true,
        preserveDrawingBuffer: false,
        powerPreference: 'high-performance'
      });

      if (!this.context) {
        console.warn('WebGL 2.0 not supported, falling back to WebGL 1.0');
        this.enabled = false;
        return false;
      }

      this.setupContext();
      this.compileShaders();
      this.setupEventListeners();

      console.log('✅ GPU Renderer initialized');
      return true;
    } catch (error) {
      console.error('GPU Renderer initialization failed:', error);
      this.enabled = false;
      return false;
    }
  }

  /**
   * Setup WebGL context
   */
  setupContext() {
    const gl = this.context;
    
    gl.clearColor(0.96, 0.96, 0.96, 1.0);
    gl.enable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    
    // Extensions
    gl.getExtension('EXT_color_buffer_float');
    gl.getExtension('OES_texture_float_linear');
  }

  /**
   * Compile vertex and fragment shaders
   */
  compileShaders() {
    const vertexSource = `#version 300 es
      layout(location = 0) in vec2 position;
      layout(location = 1) in vec2 texCoord;
      
      out vec2 vTexCoord;
      
      uniform mat4 projection;
      
      void main() {
        gl_Position = projection * vec4(position, 0.0, 1.0);
        vTexCoord = texCoord;
      }
    `;

    const fragmentSource = `#version 300 es
      precision highp float;
      
      in vec2 vTexCoord;
      out vec4 outColor;
      
      uniform sampler2D texture0;
      
      void main() {
        outColor = texture(texture0, vTexCoord);
      }
    `;

    this.program = this.createProgram(vertexSource, fragmentSource);
    this.shaders.set('default', this.program);
  }

  /**
   * Create shader program
   */
  createProgram(vertexSource, fragmentSource) {
    const gl = this.context;
    
    const vertexShader = this.compileShader(vertexSource, gl.VERTEX_SHADER);
    const fragmentShader = this.compileShader(fragmentSource, gl.FRAGMENT_SHADER);
    
    const program = gl.createProgram();
    gl.attachShader(program, vertexShader);
    gl.attachShader(program, fragmentShader);
    gl.linkProgram(program);
    
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      console.error('Program linking failed:', gl.getProgramInfoLog(program));
      return null;
    }
    
    return program;
  }

  /**
   * Compile individual shader
   */
  compileShader(source, type) {
    const gl = this.context;
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      console.error('Shader compilation failed:', gl.getShaderInfoLog(shader));
      return null;
    }
    
    return shader;
  }

  /**
   * Create texture from image
   */
  async createTexture(name, imageUrl) {
    const gl = this.context;
    const texture = gl.createTexture();
    
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    
    return new Promise((resolve, reject) => {
      const image = new Image();
      image.crossOrigin = 'anonymous';
      image.onload = () => {
        gl.bindTexture(gl.TEXTURE_2D, texture);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, image);
        this.textures.set(name, texture);
        resolve(texture);
      };
      image.onerror = reject;
      image.src = imageUrl;
    });
  }

  /**
   * Render frame
   */
  render(callback) {
    if (!this.enabled || !this.context) {
      return;
    }

    const gl = this.context;
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
    
    if (typeof callback === 'function') {
      callback(gl, this.program);
    }
    
    this.updateMetrics();
  }

  /**
   * Start animation loop
   */
  startRenderLoop(callback) {
    if (!this.enabled) return;
    
    const loop = () => {
      this.render(callback);
      requestAnimationFrame(loop);
    };
    
    loop();
  }

  /**
   * Update performance metrics
   */
  updateMetrics() {
    this.frameCount++;
    const now = performance.now();
    const elapsed = now - this.lastTime;
    
    if (elapsed >= 1000) {
      this.fps = Math.round((this.frameCount * 1000) / elapsed);
      this.frameCount = 0;
      this.lastTime = now;
    }
  }

  /**
   * Get current FPS
   */
  getFPS() {
    return this.fps;
  }

  /**
   * Handle window resize
   */
  setupEventListeners() {
    window.addEventListener('resize', () => {
      this.canvas.width = window.innerWidth;
      this.canvas.height = window.innerHeight;
      
      const gl = this.context;
      gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    });
  }

  /**
   * Get WebGL stats
   */
  getStats() {
    const gl = this.context;
    if (!gl) return null;
    
    return {
      vendor: gl.getParameter(gl.VENDOR),
      renderer: gl.getParameter(gl.RENDERER),
      version: gl.getParameter(gl.VERSION),
      fps: this.fps,
      textureCount: this.textures.size,
      shaderCount: this.shaders.size
    };
  }

  /**
   * Cleanup
   */
  destroy() {
    if (this.context) {
      this.context.getExtension('WEBGL_lose_context').loseContext();
    }
    this.textures.clear();
    this.shaders.clear();
    this.enabled = false;
  }
}

export { GPURenderer };
