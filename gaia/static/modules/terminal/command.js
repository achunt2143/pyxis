/**
 * Commands Component
 * File: gaia/static/modules/terminal/commands.js
 * Purpose: Command processing
 */
class CommandProcessor {
  constructor(shell) {
    this.shell = shell;
  }

  process(input) {
    const trimmed = input.trim();
    if (!trimmed) return;

    this.shell.printPrompt();
    this.shell.print(trimmed);
    this.shell.execute(trimmed);
  }

  async executeAsync(command) {
    return new Promise((resolve) => {
      setTimeout(() => {
        this.shell.execute(command);
        resolve();
      }, 100);
    });
  }
}

export default CommandProcessor;