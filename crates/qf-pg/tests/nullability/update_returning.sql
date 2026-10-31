-- expect: id=notnull notes=nullable:expression email=notnull
UPDATE invoices i SET notes = $1 FROM customers c
WHERE c.id = i.customer_id AND i.id = $2
RETURNING i.id, i.notes, c.email
