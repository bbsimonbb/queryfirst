-- expect: customer_id=nullable:analyzer-gave-up total=notnull
SELECT customer_id, total FROM addresses JOIN invoices USING (customer_id)
