/**
 * Metadata Component
 * File: gaia/static/modules/media-player/metadata.js
 * Purpose: Track metadata display
 */
class MetadataDisplay {
  constructor(options) {
    this.container = options.container;
  }

  render(track) {
    const html = `
      <div class="metadata-display">
        <div class="metadata-title">${track.title}</div>
        <div class="metadata-artist">${track.artist}</div>
        <div class="metadata-album">${track.album || 'Unknown Album'}</div>
        <div class="metadata-duration">
          <span class="duration-current">0:00</span> / 
          <span class="duration-total">${track.duration}</span>
        </div>
      </div>
    `;

    this.container.innerHTML = html;
  }

  update(currentTime, totalTime) {
    const current = this.container.querySelector('.duration-current');
    if (current) {
      current.textContent = this.formatTime(currentTime);
    }
  }

  formatTime(seconds) {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  }
}

export default MetadataDisplay;