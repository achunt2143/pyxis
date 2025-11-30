/**
 * Shell Component
 * File: gaia/static/modules/terminal/shell.js
 * Purpose: Shell emulation
 */
class Shell {
  constructor(options) {
    this.outputElement = options.outputElement;
    this.inputElement = options.inputElement;
    this.history = [];
    this.currentDir = '/home/user';
    this.initCommands();
  }

  initCommands() {
    this.commands = {
      help: () => this.help(),
      pwd: () => this.pwd(),
      ls: (args) => this.ls(args),
      cd: (args) => this.cd(args),
      clear: () => this.clear(),
      echo: (args) => args.join(' '),
      date: () => new Date().toString(),
    };
  }

  execute(command) {
    const [cmd, ...args] = command.split(' ').filter(c => c);
    this.history.push(command);

    if (this.commands[cmd]) {
      try {
        const result = this.commands[cmd](args);
        this.print(result);
      } catch (error) {
        this.print(`Error: ${error.message}`);
      }
    } else if (cmd) {
      this.print(`Command not found: ${cmd}`);
    }
  }

  help() {
    return `Available commands:
  help    - Show this help message
  pwd     - Print working directory
  ls      - List directory contents
  cd      - Change directory
  clear   - Clear the screen
  echo    - Echo text
  date    - Show current date/time`;
  }

  pwd() {
    return this.currentDir;
  }

  ls(args) {
    return `total 12
drwxr-xr-x  3 user  staff   96 Nov 30 09:00 Desktop
drwxr-xr-x  5 user  staff  160 Nov 30 09:00 Documents
drwxr-xr-x  4 user  staff  128 Nov 30 09:00 Downloads`;
  }

  cd(args) {
    if (args.length === 0) {
      this.currentDir = '/home/user';
    } else {
      const dir = args[0];
      if (dir === '..') {
        const parts = this.currentDir.split('/').slice(0, -1);
        this.currentDir = parts.join('/') || '/';
      } else if (dir.startsWith('/')) {
        this.currentDir = dir;
      } else {
        this.currentDir = `${this.currentDir}/${dir}`;
      }
    }
    return '';
  }

  clear() {
    this.outputElement.innerHTML = '';
    return '';
  }

  print(text) {
    const line = document.createElement('div');
    line.textContent = text;
    this.outputElement.appendChild(line);
    this.outputElement.scrollTop = this.outputElement.scrollHeight;
  }

  printPrompt() {
    const prompt = document.createElement('div');
    prompt.textContent = `$ `;
    this.outputElement.appendChild(prompt);
  }
}

export default Shell;