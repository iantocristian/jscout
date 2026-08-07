export function authorizePayment(orderId: string, amount: number) {
  return paymentGateway.authorize({ orderId, amount });
}
