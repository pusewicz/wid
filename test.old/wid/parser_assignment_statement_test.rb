module Wid
  class ParserAssignmentStatementTest < ParserTest
    def test_assignment_statement
      ast = parse("foo = 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::AssignmentExpression,
              operator: "=",
              left: {
                class: Nodes::Identifier,
                name: "foo"
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 1
              }
            }
          }
        ]
      }, ast)
    end

    def test_assignment_statement_chained
      ast = parse("foo = bar = 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::AssignmentExpression,
              operator: "=",
              left: {
                class: Nodes::Identifier,
                name: "foo"
              },
              right: {
                class: Nodes::AssignmentExpression,
                operator: "=",
                left: {
                  class: Nodes::Identifier,
                  name: "bar"
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 1
                }
              }
            }
          }
        ]
      }, ast)
    end
  end
end
