-- expect: notes=nullable email=required:where-eq-notnull
UPDATE invoices i SET notes = :notes FROM customers c WHERE c.id = i.customer_id AND c.email = :email
