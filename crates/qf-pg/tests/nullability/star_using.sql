-- expect: id=nullable:analyzer-gave-up email=nullable:analyzer-gave-up name=nullable:analyzer-gave-up access=nullable:analyzer-gave-up tags=nullable:analyzer-gave-up prefs=nullable:analyzer-gave-up created_at=nullable:analyzer-gave-up birthday=nullable:analyzer-gave-up
SELECT * FROM customers JOIN (SELECT id FROM customers) s USING (id)
