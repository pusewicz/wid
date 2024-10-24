module Wid
  class ParserMemberTest < ParserTest
    def test_member_property
      ast = parse("foo.bar")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::MemberExpression,
              computed: false,
              object: {
                class: Nodes::Identifier,
                name: "foo"
              },
              property: {
                class: Nodes::Identifier,
                name: "bar"
              }
            }
          }
        ]
      }, ast)
    end

    def test_member_property_assign
      ast = parse("foo.bar = 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::AssignmentExpression,
              operator: "=",
              left: {
                class: Nodes::MemberExpression,
                computed: false,
                object: {
                  class: Nodes::Identifier,
                  name: "foo"
                },
                property: {
                  class: Nodes::Identifier,
                  name: "bar"
                }
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

    def test_member_computed_property_assign
      ast = parse("foo[0] = 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::AssignmentExpression,
              operator: "=",
              left: {
                class: Nodes::MemberExpression,
                computed: true,
                object: {
                  class: Nodes::Identifier,
                  name: "foo"
                },
                property: {
                  class: Nodes::NumericLiteral,
                  value: 0
                }
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

    def test_member_chained_access
      ast = parse("a.b.c['d']")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::MemberExpression,
              computed: true,
              object: {
                class: Nodes::MemberExpression,
                computed: false,
                object: {
                  class: Nodes::MemberExpression,
                  computed: false,
                  object: {
                    class: Nodes::Identifier,
                    name: "a"
                  },
                  property: {
                    class: Nodes::Identifier,
                    name: "b"
                  }
                },
                property: {
                  class: Nodes::Identifier,
                  name: "c"
                }
              },
              property: {
                class: Nodes::StringLiteral,
                value: "d"
              }
            }
          }
        ]
      }, ast)
    end
  end
end
