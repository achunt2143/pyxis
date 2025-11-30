/**
 * Favorites Component
 * File: gaia/static/modules/sidebar/favorites.js
 */
class Favorites {
  constructor(options) {
    this.container = options.container;
  }

  async render() {
    this.container.innerHTML = `
      <div class="favorites-section">
        <div class="favorite-item">⭐</div>
        <div class="favorite-item">❤️</div>
      </div>
    `;
  }
}
export default Favorites;