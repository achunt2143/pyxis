/**
 * Visualizer Component
 * File: gaia/static/modules/media-player/visualizer.js
 * Purpose: Audio visualizer
 */
class AudioVisualizer {
  constructor(options) {
    this.container = options.container;
    this.canvas = document.createElement('canvas');
    this.ctx = this.canvas.getContext('2d');
    this.analyser = null;
    this.bars = 64;
    this.animationId = null;
  }

  init() {
    this.canvas.width = this.container.offsetWidth;
    this.canvas.height = this.container.offsetHeight;
    this.container.appendChild(this.canvas);

    this.animate();
  }

  animate() {
    this.ctx.fillStyle = 'rgba(32, 128, 148, 0.1)';
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);

    this.drawBars();
    this.animationId = requestAnimationFrame(() => this.animate());
  }

  drawBars() {
    const barWidth = this.canvas.width / this.bars;
    const baseHeight = this.canvas.height / 2;

    for (let i = 0; i < this.bars; i++) {
      const height = Math.random() * baseHeight;
      const x = i * barWidth;

      this.ctx.fillStyle = `hsl(185, 100%, ${50 + height / baseHeight * 20}%)`;
      this.ctx.fillRect(x, this.canvas.height - height, barWidth - 2, height);
    }
  }

  stop() {
    if (this.animationId) {
      cancelAnimationFrame(this.animationId);
    }
  }
}

export default AudioVisualizer;