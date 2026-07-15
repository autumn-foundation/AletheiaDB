:begin
CREATE (:`Person`:`UNIQUE IMPORT LABEL` {`name`:"Alice", `age`:30, `active`:true, `UNIQUE IMPORT ID`:0});
CREATE (:`Person`:`UNIQUE IMPORT LABEL` {`name`:"Bob", `age`:25, `score`:9.5, `UNIQUE IMPORT ID`:1});
CREATE (:`City`:`UNIQUE IMPORT LABEL` {`name`:"Paris", `UNIQUE IMPORT ID`:2});
:commit
:begin
MATCH (n1:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}), (n2:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:1}) CREATE (n1)-[:`KNOWS` {`since`:2020}]->(n2);
MATCH (n1:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}), (n2:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:2}) CREATE (n1)-[:`LIVES_IN`]->(n2);
:commit
