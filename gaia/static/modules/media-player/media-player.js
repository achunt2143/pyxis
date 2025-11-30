/**
 * Media Player Component
 * File: gaia/static/modules/media-player/media-player.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class MediaPlayer {
  constructor(options) {
    this.element = options.element;
    this.services = options.services;
    this.currentTrack = null;
    this.isPlaying = false;
    this.playlist = [];
  }

  async initialize() {
    await ModuleLoader.loadStyles('media-player');
    this.createStructure();
    this.attachListeners();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="media-player-container">
        <div class="album-art" id="album-art"></div>
        <div class="track-info" id="track-info"></div>
        <div class="controls" id="controls"></div>
        <div class="playlist" id="playlist"></div>
        <div class="visualizer" id="visualizer"></div>
      </div>
    `;
    this.createControls();
  }

  createControls() {
    const controlsEl = document.getElementById('controls');
    controlsEl.innerHTML = `
      <button id="prev-btn">⏮️</button>
      <button id="play-btn">▶️</button>
      <button id="next-btn">⏭️</button>
      <input type="range" id="progress-bar" min="0" max="100" value="0">
    `;
  }

  attachListeners() {
    document.getElementById('play-btn').addEventListener('click', () => this.togglePlayPause());
    document.getElementById('prev-btn').addEventListener('click', () => this.previousTrack());
    document.getElementById('next-btn').addEventListener('click', () => this.nextTrack());
  }

  togglePlayPause() {
    this.isPlaying = !this.isPlaying;
    const btn = document.getElementById('play-btn');
    btn.textContent = this.isPlaying ? '⏸️' : '▶️';
  }

  previousTrack() {
    console.log('Previous track');
  }

  nextTrack() {
    console.log('Next track');
  }

  async destroy() {}
}
export default MediaPlayer;