export function startReceiptWorker() {
  orderBus.on("order.created", sendReceipt);
}

function sendReceipt(orderId: string) {
  return mailer.send({ template: "receipt", orderId });
}
