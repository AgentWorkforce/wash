export interface TaxRate {
  rate: number;
  jurisdiction: string;
}

export function lookupRate(j: string): TaxRate {
  return { rate: 0.08, jurisdiction: j };
}
