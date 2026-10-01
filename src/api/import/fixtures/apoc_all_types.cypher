:begin
CREATE (:`Person`:`UNIQUE IMPORT LABEL` {`name`:"Alice", `initial`:"A", `age`:30, `big`:9999999999, `score`:9.5, `active`:true, `born`:date('1990-05-01'), `created`:datetime('2020-01-01T00:00:00Z'), `last_seen`:localdatetime('2021-06-01T12:00:00'), `tags`:["x","y"], `emb`:[0.1,0.2,0.3], `UNIQUE IMPORT ID`:0});
CREATE (:`Person`:`UNIQUE IMPORT LABEL` {`name`:"Bob", `age`:25, `active`:false, `UNIQUE IMPORT ID`:1});
CREATE (:`City`:`UNIQUE IMPORT LABEL` {`name`:"Paris", `founded`:date('0300-01-01'), `population`:2000000, `UNIQUE IMPORT ID`:2});
:commit
:begin
MATCH (n1:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}), (n2:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:1}) CREATE (n1)-[:`KNOWS` {`since`:2020, `weight`:0.8}]->(n2);
MATCH (n1:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:1}), (n2:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}) CREATE (n1)-[:`KNOWS` {`since`:2021}]->(n2);
MATCH (n1:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}), (n2:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:2}) CREATE (n1)-[:`LIVES_IN`]->(n2);
:commit
