module Wid
  class ParserTest < Test
    def test_whitespace_skip
      ast = parse("    1 ")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 1
            }]
          }
        ]
      }, ast)
    end

    def test_comment_skip
      ast = parse("# This is a comment\n1")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 1
            }]
          }
        ]
      }, ast)
    end

    def test_comment_after_expression
      ast = parse("1 # This is a comment")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 1
            }]
          }
        ]
      }, ast)
    end

    def test_parse_integer_number
      ast = parse("1")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 1
            }]
          }
        ]
      }, ast)
    end

    def test_parse_float_number
      ast = parse("1.53")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 1.53
            }]
          }
        ]
      }, ast)
    end

    def test_parse_double_quote_string
      ast = parse('"hello"')

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::StringLiteral,
              value: "hello"
            }]
          }
        ]
      }, ast)
    end

    def test_parse_single_quote_string
      ast = parse("'hello'")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::StringLiteral,
              value: "hello"
            }]
          }
        ]
      }, ast)
    end

    def test_parse_number_as_string
      ast = parse('"1"')

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::StringLiteral,
              value: "1"
            }]
          }
        ]
      }, ast)
    end

    def test_parse_string_with_spaces
      ast = parse('"hello world"')

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::StringLiteral,
              value: "hello world"
            }]
          }
        ]
      }, ast)
    end

    def test_statement_list
      ast = parse("1; 2; 3;")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 1
            }]
          },
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 2
            }]
          },
          {
            class: Nodes::ExpressionStatement,
            children: [{
              class: Nodes::NumericLiteral,
              value: 3
            }]
          }
        ]
      }, ast)
    end

    def test_block_statement_curly_braces
      ast = parse("{ 1; 2; 3 }")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::BlockStatement,
            children: [
              {
                class: Nodes::ExpressionStatement,
                children: [{
                  class: Nodes::NumericLiteral,
                  value: 1
                }]
              },
              {
                class: Nodes::ExpressionStatement,
                children: [{
                  class: Nodes::NumericLiteral,
                  value: 2
                }]
              },
              {
                class: Nodes::ExpressionStatement,
                children: [{
                  class: Nodes::NumericLiteral,
                  value: 3
                }]
              }
            ]
          }
        ]
      }, ast)
    end

    def test_block_statement_do_end
      ast = parse("do 1; 2; 3 end")

      assert_ast_equal({
        class: Nodes::Program,
        children: [
          {
            class: Nodes::BlockStatement,
            children: [
              {
                class: Nodes::ExpressionStatement,
                children: [{
                  class: Nodes::NumericLiteral,
                  value: 1
                }]
              },
              {
                class: Nodes::ExpressionStatement,
                children: [{
                  class: Nodes::NumericLiteral,
                  value: 2
                }]
              },
              {
                class: Nodes::ExpressionStatement,
                children: [{
                  class: Nodes::NumericLiteral,
                  value: 3
                }]
              }
            ]
          }
        ]
      }, ast)
    end

    private

    def assert_ast_equal(expected, actual)
      assert_equal(expected[:class], actual.class)

      expected.each do |key, value|
        next if key == :children || key == :class
        assert_respond_to(actual, key)
        assert_equal(value, actual.send(key))
      end

      if expected.has_key?(:children)
        expected[:children].each_with_index do |child, index|
          assert_ast_equal(child, actual.children[index])
        end
      end
    end

    def parse(input)
      tokens = Lexer.tokenize(input)
      Parser.parse(tokens)
    end

    def dump(ast, indent = 0, output = [])
      ast.children.each do |child|
        output << "#{" " * indent}#{child.inspect}"
        dump(child, indent + 2)
      end

      output.join("\n")
    end
  end
end
