-- expect: l=nullable:analyzer-gave-up g=nullable:analyzer-gave-up row=nullable:analyzer-gave-up
SELECT lower(email) AS l, g, c AS row FROM customers c, generate_series(1, 2) g
