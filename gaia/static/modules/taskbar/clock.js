/**
 * Clock Component
 * File: gaia/static/modules/taskbar/clock.js
 */
class Clock {
  constructor(options) {
    this.container = options.container;
    this.intervalId = null;
  }

  start() {
    this.update();
    this.intervalId = setInterval(() => this.update(), 1000);
  }

  update() {
    const now = new Date();
    const time = now.toLocaleTimeString('en-US', {
      hour: '2-digit',
      minute: '2-digit',
      hour12: true
    });
    this.container.textContent = time;
  }

  stop() {
    clearInterval(this.intervalId);
  }
}
export default Clock;