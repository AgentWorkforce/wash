export interface LineItem {
  sku: string;
  qty: number;
  unitPrice: number;
  lineTotal?: number;
}

export interface Cart {
  id: string;
  customerId: string;
  items: LineItem[];
}
