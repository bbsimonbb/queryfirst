-- expect: total=notnull address_id=nullable:expression
DELETE FROM invoices WHERE id = $1 RETURNING total, address_id
