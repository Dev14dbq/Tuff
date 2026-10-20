execute if function t:ret run tellraw @s "ret passed"
execute if function t:retfail run tellraw @s "retfail passed"
execute if function t:noret run tellraw @s "noret passed"
