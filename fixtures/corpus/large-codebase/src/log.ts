export class Logger {
  constructor(private readonly tag: string) {}
  debug(msg: string, meta?: object): void {
    // no-op in fixture
  }
  trace(msg: string): void {
    // no-op in fixture
  }
}
