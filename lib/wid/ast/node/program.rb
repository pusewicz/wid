module Wid
  module AST
    class Node
      class Program < Node
        attr_reader :statements

        def initialize(statements:)
          @statements = statements
        end

        def to_h
          super.merge(statements: @statements.to_h)
        end
      end
    end
  end
end
